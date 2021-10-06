#[macro_use]
extern crate serde_json;
#[macro_use]
extern crate serde_derive;

use std::time::Duration;

use anyhow::{bail, Error};

const GH_TOKEN: &str = include_str!("../gh.token");
const MX_TOKEN: &str = include_str!("../mx.token");

#[derive(Deserialize, Debug, Clone)]
struct GithubSearchResult {
    total_count: i64,
}

#[derive(Debug, Clone)]
struct PendingReviewChecker {
    client: reqwest::Client,
}

impl PendingReviewChecker {
    pub fn new() -> PendingReviewChecker {
        PendingReviewChecker {
            client: reqwest::Client::new(),
        }
    }

    async fn get_review_count(&self) -> Result<i64, Error> {
        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aopen%20is%3Apr%20team-review-requested%3Amatrix-org%2Fsynapse-core")
            .basic_auth("erikjohnston", Some(GH_TOKEN.trim()))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .header("User-Agent", "github-project-bot")
            .send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await?;
            bail!("Got non-200 from GH: {}, text: {}", status, text);
        }

        let search: GithubSearchResult = resp.json().await?;

        Ok(search.total_count)
    }

    async fn get_ps_column_count(&self) -> Result<i64, Error> {
        let resp = self
            .client
            .get("https://api.github.com/projects/columns/13411398/cards")
            .basic_auth("erikjohnston", Some(GH_TOKEN.trim()))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .header("User-Agent", "github-project-bot")
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

    async fn update_state(&self, review_count: i64, ps_column_count: i64) -> Result<(), Error> {
        let severity = if review_count > 0 {
            "warning"
        } else {
            "normal"
        };

        self.client.put("https://jki.re/_matrix/client/r0/rooms/!SGNQGPGUwtcPBUotTL:matrix.org/state/re.jki.counter/gh_reviews")
            .header("Authorization", format!("Bearer {}", MX_TOKEN.trim()))
            .json(&json!({
                "title": "Pending reviews",
                "value": review_count,
                "severity": severity,
                "link": "https://github.com/pulls/review-requested",
            }))
            .send().await?;

        self.client.put("https://jki.re/_matrix/client/r0/rooms/!SGNQGPGUwtcPBUotTL:matrix.org/state/re.jki.counter/gh_ps_asks")
            .header("Authorization", format!("Bearer {}", MX_TOKEN.trim()))
            .json(&json!({
                "title": "Urgent PS Tasks Column",
                "value": ps_column_count,
                "severity": if ps_column_count > 0 {"warning"} else { "normal"},
                "link": "https://github.com/orgs/matrix-org/projects/36#column-13411398",
            }))
            .send().await?;

        Ok(())
    }

    async fn do_check_inner(&self) -> Result<(), Error> {
        let review_count = self.get_review_count().await?;
        let ps_column_count = self.get_ps_column_count().await?;

        println!(
            "There are {} pending reviews and {} in ps column",
            review_count, ps_column_count
        );

        self.update_state(review_count, ps_column_count).await?;

        Ok(())
    }

    pub async fn do_check(&self) {
        self.do_check_inner().await.unwrap()
    }
}

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    let checker = PendingReviewChecker::new();

    let c = checker.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            c.do_check().await;
            interval.tick().await;
        }
    });

    let make_service = hyper::service::make_service_fn(move |_| {
        let checker = checker.clone();
        async move {
            Ok::<_, hyper::Error>(hyper::service::service_fn(move |_req| {
                let checker = checker.clone();
                async move {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    checker.do_check().await;
                    Ok::<_, hyper::Error>(hyper::Response::new(hyper::Body::from("Done")))
                }
            }))
        }
    });

    // Then bind and serve...
    hyper::Server::bind(&"127.0.0.1:8088".parse().unwrap())
        .serve(make_service)
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    Ok(())
}
