#[macro_use]
extern crate serde_json;
#[macro_use]
extern crate serde_derive;

use std::{env, time::Duration};

use anyhow::{bail, Error};

#[derive(Deserialize, Debug, Clone)]
struct GithubSearchResult {
    total_count: i64,
}

#[derive(Debug, Clone)]
struct PendingReviewChecker {
    client: reqwest::Client,
    matrix_token: String,
    github_username: String,
    github_token: String,
}

impl PendingReviewChecker {
    async fn get_review_count(&self) -> Result<i64, Error> {
        let resp = self.client.get("https://api.github.com/search/issues?q=is%3Aopen%20is%3Apr%20team-review-requested%3Amatrix-org%2Fsynapse-core")
            .basic_auth(&self.github_username, Some(&self.github_token))
            .header("Accept", "application/vnd.github.inertia-preview+json")
            .header("User-Agent", "github-project-bot")
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
            .header("User-Agent", "github-project-bot")
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
            .header("Authorization", format!("Bearer {}", self.matrix_token))
            .json(&json!({
                "title": "Pending reviews",
                "value": review_count,
                "severity": severity,
                "link": "https://github.com/pulls/review-requested",
            }))
            .send().await?;

        self.client.put("https://jki.re/_matrix/client/r0/rooms/!SGNQGPGUwtcPBUotTL:matrix.org/state/re.jki.counter/gh_ps_asks")
            .header("Authorization", format!("Bearer {}", self.matrix_token))
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
    let client = reqwest::Client::new();

    let matrix_token = env::var("MX_TOKEN").expect("valid mx token");
    let github_username = env::var("GH_USER").expect("valid gh username");
    let github_token = env::var("GH_TOKEN").expect("valid gh token");

    let checker = PendingReviewChecker {
        client,
        matrix_token,
        github_username,
        github_token,
    };

    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        checker.do_check().await;
        interval.tick().await;
    }
}
