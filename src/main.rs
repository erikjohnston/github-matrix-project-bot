#[macro_use]
extern crate serde_json;
#[macro_use]
extern crate serde_derive;

use std::time::Duration;

use actix::{Actor, AsyncContext};
use actix_web::{server, App};
use futures::Future;

const GH_TOKEN: &str = include_str!("../gh.token");
const MX_TOKEN: &str = include_str!("../mx.token");

#[derive(Deserialize, Debug, Clone)]
struct GithubSearchResult {
    total_count: i64,
}

#[derive(Debug, Clone)]
struct PendingReviewChecker {
    client: reqwest::r#async::Client,
}

impl PendingReviewChecker {
    pub fn new() -> PendingReviewChecker {
        PendingReviewChecker {
            client: reqwest::r#async::Client::new(),
        }
    }

    fn get_review_count(
        &self,
    ) -> impl Future<Item = i64, Error = Box<dyn std::error::Error + 'static>> {
        self.client.get("https://api.github.com/search/issues?q=is%3Aopen%20is%3Apr%20team-review-requested%3Amatrix-org%2Fsynapse-core")
            .basic_auth("erikjohnston", Some(GH_TOKEN.trim()))
            .send()
            .and_then(|mut resp| resp.json())
            .map(|search: GithubSearchResult| {
                search.total_count
            })
            .map_err(|e| e.into())
    }

    fn update_state(
        &self,
        counter: i64,
    ) -> impl Future<Item = (), Error = Box<dyn std::error::Error + 'static>> {
        let severity = if counter > 0 { "warning" } else { "normal" };

        self.client.put("https://jki.re/_matrix/client/r0/rooms/!GebUmESDHVsWJQSBSX:jki.re/state/re.jki.counter/gh_reviews")
            .header("Authorization", format!("Bearer {}", MX_TOKEN.trim()))
            .json(&json!({
                "title": "Pending reviews",
                "value": counter,
                "severity": severity,
                "link": "https://github.com/pulls/review-requested",
            }))
            .send()
            .map(|_resp| ())
            .map_err(|e| e.into())
    }

    pub fn do_check(&self) -> impl Future<Item = (), Error = ()> {
        let self_clone = self.clone();

        self.get_review_count()
            .and_then(move |count| {
                println!("There are {} pending reviews", count);

                self_clone.update_state(count)
            })
            .map_err(|err| {
                eprintln!("Error: {}", err);
            })
    }
}

impl Actor for PendingReviewChecker {
    type Context = actix::Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        actix::spawn(self.do_check().or_else(|_| Ok(())));

        let c = self.clone();
        ctx.run_interval(Duration::from_secs(30), move |_, _| {
            actix::spawn(c.do_check().or_else(|_| Ok(())))
        });
    }
}

fn main() {
    let checker = PendingReviewChecker::new();
    checker.clone().start();

    server::new(move || {
        App::with_state(checker.clone()).resource("/", |r| {
            r.f(move |req| {
                tokio::spawn(req.state().do_check());

                "Done"
            })
        })
    })
    .shutdown_timeout(10)
    .workers(1)
    .bind("127.0.0.1:8088")
    .unwrap()
    .run();
}
