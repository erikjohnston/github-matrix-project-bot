#[macro_use]
extern crate serde_json;
#[macro_use]
extern crate serde_derive;

use std::{env, time::Duration};

use actix_web::{error::ErrorInternalServerError, get, route, web::Data, App, HttpServer};
use anyhow::{bail, Error};
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
}

impl PendingReviewChecker {
    async fn get_review_count(&self) -> Result<i64, Error> {
        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aopen%20is%3Apr%20team-review-requested%3Amatrix-org%2Fsynapse-core")
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

        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aopen%20is%3Apr%20team-review-requested%3Avector-im%2Fsynapse-core")
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

    async fn get_ps_column_count(&self) -> Result<i64, Error> {
        let resp = self
            .client
            .get("https://api.github.com/projects/columns/13411398/cards")
            .basic_auth(&self.github_username, Some(&self.github_token))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from GH: {}, text: {}", status, text);
        }

        let resp: serde_json::Value = resp.json().await?;

        let cards: Vec<serde_json::Value> = serde_json::from_value(resp)?;

        Ok(cards.len() as i64)
    }

    async fn get_untriaged_count(&self) -> Result<i64, Error> {
        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aissue+is%3Aopen+-label%3AT-Other++-label%3AT-Task+-label%3AT-Enhancement+-label%3AT-Defect+updated%3A>%3D2021-04-01+sort%3Aupdated-desc++-label%3AX-Needs-Info")
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
        ps_column_count: i64,
        untriaged_count: i64,
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

        let resp =self.client.put(format!("{}/_matrix/client/r0/rooms/!SGNQGPGUwtcPBUotTL:matrix.org/state/re.jki.counter/gh_ps_asks", self.matrix_server_url))
            .header("Authorization", format!("Bearer {}", self.matrix_token))
            .json(&json!({
                "title": "Urgent PS Tasks Column",
                "value": ps_column_count,
                "severity": if ps_column_count > 0 {"warning"} else { "normal"},
                "link": "https://github.com/orgs/matrix-org/projects/36#column-13411398",
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
                "link": "https://github.com/matrix-org/synapse/issues?q=is%3Aissue+is%3Aopen+-label%3AT-Other++-label%3AT-Task+-label%3AT-Enhancement+-label%3AT-Defect+updated%3A%3E%3D2021-04-01+sort%3Aupdated-desc++-label%3AX-Needs-Info",
            }))
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from MX: {}, text: {}", status, text);
        }

        Ok(())
    }

    async fn do_check(&self) -> Result<(), Error> {
        let review_count = self.get_review_count().await?;
        let ps_column_count = self.get_ps_column_count().await?;
        let untriaged_count = self.get_untriaged_count().await?;

        info!(
            "There are {} pending reviews and {} in ps column",
            review_count, ps_column_count
        );

        self.update_state(review_count, ps_column_count, untriaged_count)
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

    let checker = PendingReviewChecker {
        client,
        matrix_server_url,
        matrix_token,
        github_username,
        github_token,
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
