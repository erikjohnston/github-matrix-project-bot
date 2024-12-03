#[macro_use]
extern crate serde_json;
#[macro_use]
extern crate serde_derive;

use std::{
    env,
    sync::{Arc, Mutex},
    time::Duration,
};

use actix_web::{error::ErrorInternalServerError, get, route, web::Data, App, HttpServer};
use anyhow::{bail, Error};
use chrono::{Timelike, Utc};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_tracing::TracingMiddleware;

use tracing::{info, Instrument};
use tracing_actix_web::TracingLogger;

#[derive(Deserialize, Debug, Clone)]
struct GithubSearchResult {
    total_count: i64,
}

#[derive(Clone)]
struct PendingReviewChecker {
    client: ClientWithMiddleware,
    matrix_server_url: String,
    matrix_token: String,
    github_username: String,
    github_token: String,

    // Last time we posted the daily update. Used to decide if we should post
    // another one at any given point.
    last_posted_daily_update: Arc<Mutex<chrono::DateTime<tzfile::ArcTz>>>,
}

impl PendingReviewChecker {
    async fn get_review_count(&self) -> Result<i64, Error> {
        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aopen%20is%3Apr%20team-review-requested%2Fmatrix-org%2Fsynapse-core")
            .basic_auth(&self.github_username, Some(&self.github_token))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from GH: {}, text: {}", status, text);
        }

        let search: GithubSearchResult = resp.json().await?;

        let mut total = search.total_count;

        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aopen%20is%3Apr%20team-review-requested%3Aelement-hq%2Fsynapse-core")
            .basic_auth(&self.github_username, Some(&self.github_token))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from GH: {}, text: {}", status, text);
        }

        let search: GithubSearchResult = resp.json().await?;

        total += search.total_count;

        Ok(total)
    }

    async fn get_untriaged_count(&self) -> Result<i64, Error> {
        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aissue+is%3Aopen+-label%3AT-Other++-label%3AT-Task+-label%3AT-Enhancement+-label%3AT-Defect+updated%3A%3E%3D2021-04-01+sort%3Aupdated-desc++-label%3AX-Needs-Info+repo%3Aelement-hq/synapse")
            .basic_auth(&self.github_username, Some(&self.github_token))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from GH: {}, text: {}", status, text);
        }

        let search: GithubSearchResult = resp.json().await?;

        Ok(search.total_count)
    }

    async fn get_release_blocker_count(&self) -> Result<i64, Error> {
        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aopen+label%3AX-Release-Blocker+repo%3Aelement-hq/synapse")
            .basic_auth(&self.github_username, Some(&self.github_token))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from GH: {}, text: {}", status, text);
        }

        let search: GithubSearchResult = resp.json().await?;

        Ok(search.total_count)
    }

    async fn get_spec_clarification_closed_count(&self) -> Result<i64, Error> {
        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aissue+label%3Aclarification+is%3Aclosed+closed%3A>2022-11-21+repo%3Amatrix-org/matrix-spec")
            .basic_auth(&self.github_username, Some(&self.github_token))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from GH: {}, text: {}", status, text);
        }

        let search: GithubSearchResult = resp.json().await?;

        Ok(search.total_count)
    }

    async fn update_state(
        &self,
        review_count: i64,
        untriaged_count: i64,
        release_blocker_count: i64,
        spec_clarification_closed_count: i64,
    ) -> Result<(), Error> {
        let severity = if review_count > 0 {
            "warning"
        } else {
            "normal"
        };

        let resp = self.client.put(format!("{}/_matrix/client/r0/rooms/!SGNQGPGUwtcPBUotTL:matrix.org/state/re.jki.counter/gh_reviews", self.matrix_server_url))
            .header("Authorization", format!("Bearer {}", self.matrix_token))
            .json(&json!({
                "title": "Pending reviews",
                "value": review_count,
                "severity": severity,
                "link": "https://github.com/pulls/review-requested",
            }))
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from MX: {}, text: {}", status, text);
        }

        let resp =self.client.put(format!("{}/_matrix/client/r0/rooms/!SGNQGPGUwtcPBUotTL:matrix.org/state/re.jki.counter/gh_untriaged", self.matrix_server_url))
            .header("Authorization", format!("Bearer {}", self.matrix_token))
            .json(&json!({
                "title": "Untriaged Synapse issues",
                "value": untriaged_count,
                "severity": "normal",
                "link": "https://github.com/element-hq/synapse/issues?q=is%3Aissue+is%3Aopen+-label%3AT-Other++-label%3AT-Task+-label%3AT-Enhancement+-label%3AT-Defect+updated%3A%3E%3D2021-04-01+sort%3Aupdated-desc++-label%3AX-Needs-Info",
            }))
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from MX: {}, text: {}", status, text);
        }

        let release_blocker_body = if release_blocker_count > 0 {
            json!({
                "title": "Synapse Release Blockers",
                "value": release_blocker_count,
                "severity": "alert",
                "link": "https://github.com/element-hq/synapse/labels/X-Release-Blocker",
            })
        } else {
            json!({})
        };

        let resp =self.client.put(format!("{}/_matrix/client/r0/rooms/!SGNQGPGUwtcPBUotTL:matrix.org/state/re.jki.counter/release_blockers", self.matrix_server_url))
            .header("Authorization", format!("Bearer {}", self.matrix_token))
            .json(&release_blocker_body)
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from MX: {}, text: {}", status, text);
        }

        let resp =self.client.put(format!("{}/_matrix/client/r0/rooms/!wugGGUJDONpiDufANH:matrix.org/state/re.jki.counter/clarifications_closed", self.matrix_server_url))
            .header("Authorization", format!("Bearer {}", self.matrix_token))
            .json(&json!({
                "title": "Spec clarifications closed",
                "value": spec_clarification_closed_count,
                "severity": "normal",
                "link": "https://github.com/matrix-org/matrix-spec/issues?q=is%3Aissue+label%3Aclarification+is%3Aclosed+closed%3A%3E2022-11-21",
            }))
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from MX: {}, text: {}", status, text);
        }

        Ok(())
    }

    async fn maybe_send_daily_udpate(
        &self,
        review_count: i64,
        release_blocker_count: i64,
    ) -> Result<(), Error> {
        let last_posted_daily_update = self
            .last_posted_daily_update
            .lock()
            .expect("poisoned")
            .clone();

        let tz = last_posted_daily_update.timezone();

        let now = Utc::now().with_timezone(&tz);
        let update_time = now
            .clone()
            .with_hour(9)
            .expect("valid hour")
            .with_minute(55)
            .expect("valid minute")
            .with_second(0)
            .expect("valid second")
            .with_nanosecond(0)
            .expect("valid nanosecond");

        if !(last_posted_daily_update < update_time && update_time < now) {
            return Ok(());
        }

        let mut body = format!(
            "Pending reviews: {review_count}\nSynapse Release Blockers: {release_blocker_count}"
        );
        let mut formatted_body = if review_count > 0 {
            format!(
                r#"<strong><a href="https://github.com/pulls/review-requested"><font color="orange">Pending reviews: {review_count}</font></a></strong>"#
            )
        } else {
            format!(r#"Pending reviews: {review_count}"#)
        };

        if release_blocker_count > 0 {
            body.push_str("\nSynapse Release Blockers: ");
            body.push_str(&release_blocker_count.to_string());

            formatted_body.push_str(&format!(
                r#"<br><strong><a href="https://github.com/element-hq/synapse/labels/X-Release-Blocker"><font color="red">Synapse Release Blockers: {release_blocker_count}</font></a></strong>"#
            ))
        }

        let txn_id = now.timestamp_millis();
        let resp =self.client.put(format!("{}/_matrix/client/r0/rooms/!SGNQGPGUwtcPBUotTL:matrix.org/send/m.room.message/{txn_id}", self.matrix_server_url))
            .header("Authorization", format!("Bearer {}", self.matrix_token))
            .json(&json!({
                "body": body,
                "msgtype": "m.text",
                "format": "org.matrix.custom.html",
                "formatted_body": formatted_body,
                "m.mentions": {},

            }))
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from MX: {}, text: {}", status, text);
        }

        *self.last_posted_daily_update.lock().expect("poisoned") = now;

        Ok(())
    }

    async fn do_check(&self) -> Result<(), Error> {
        let review_count = self.get_review_count().await?;
        let untriaged_count = self.get_untriaged_count().await?;
        let release_blocker_count = self.get_release_blocker_count().await?;
        let spec_clarification_closed_count = self.get_spec_clarification_closed_count().await?;

        info!(
            "There are {} pending reviews, {} untriaged, {} release blockers and {} closed clarifications",
            review_count, untriaged_count, release_blocker_count, spec_clarification_closed_count,
        );

        self.maybe_send_daily_udpate(review_count, release_blocker_count)
            .await?;

        self.update_state(
            review_count,
            untriaged_count,
            release_blocker_count,
            spec_clarification_closed_count,
        )
        .await?;

        Ok(())
    }
}

#[get("/health")]
async fn health() -> &'static str {
    "OK"
}

#[route("/webhook", method = "GET", method = "POST")]
async fn webhook(checker: Data<PendingReviewChecker>) -> Result<&'static str, actix_web::Error> {
    checker.do_check().await.map_err(ErrorInternalServerError)?;

    Ok("OK")
}

#[actix_web::main]
async fn main() -> Result<(), std::io::Error> {
    tracing_subscriber::fmt::init();

    let client = ClientBuilder::new(
        reqwest::Client::builder()
            .user_agent("github-project-bot")
            .build()
            .unwrap(),
    )
    .with(TracingMiddleware)
    .build();

    let mut matrix_server_url = env::var("MX_URL").expect("valid mx url");
    let matrix_token = env::var("MX_TOKEN").expect("valid mx token");
    let github_username = env::var("GH_USER").expect("valid gh username");
    let github_token = env::var("GH_TOKEN").expect("valid gh token");

    matrix_server_url = matrix_server_url.trim_end_matches('/').to_string();

    let tz = tzfile::ArcTz::named("Europe/London").expect("valid tz");

    let checker = PendingReviewChecker {
        client,
        matrix_server_url,
        matrix_token,
        github_username,
        github_token,

        // Assume we've already posted today's update on startup.
        last_posted_daily_update: Arc::new(Mutex::new(Utc::now().with_timezone(&tz))),
    };

    let c = checker.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            c.do_check()
                .instrument(tracing::info_span!("loop_iteration"))
                .await
                .ok();
            interval.tick().await;
        }
    });

    HttpServer::new(move || {
        App::new()
            .app_data(Data::new(checker.clone()))
            .wrap(TracingLogger::default())
            .service(health)
            .service(webhook)
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
