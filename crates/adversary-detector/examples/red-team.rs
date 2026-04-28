use adversary_detector::{AdversaryScanner, ScanContext, ScanVerdict, ScannerConfig};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    #[serde(default = "default_url")]
    url: String,
    #[serde(default)]
    context: FixtureContext,
    content: String,
    #[serde(default)]
    layer: FixtureLayer,
    #[serde(default)]
    expect_local: Option<FixtureVerdict>,
}

impl Fixture {
    fn expected_for_local(&self) -> FixtureVerdict {
        self.expect_local.unwrap_or(match self.layer {
            FixtureLayer::Local | FixtureLayer::Shared => FixtureVerdict::Review,
            FixtureLayer::Remote => FixtureVerdict::Clean,
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum FixtureContext {
    #[default]
    WebFetch,
    WebSearch,
    Email,
    Api,
    Exec,
}

impl From<FixtureContext> for ScanContext {
    fn from(value: FixtureContext) -> Self {
        match value {
            FixtureContext::WebFetch => ScanContext::WebFetch,
            FixtureContext::WebSearch => ScanContext::WebSearch,
            FixtureContext::Email => ScanContext::Email,
            FixtureContext::Api => ScanContext::Api,
            FixtureContext::Exec => ScanContext::Exec,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum FixtureLayer {
    #[default]
    Local,
    Remote,
    Shared,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FixtureVerdict {
    Clean,
    Review,
    Unsafe,
}

impl FixtureVerdict {
    fn matches(self, verdict: &ScanVerdict) -> bool {
        matches!(
            (self, verdict),
            (FixtureVerdict::Clean, ScanVerdict::Clean)
                | (FixtureVerdict::Review, ScanVerdict::Review { .. })
                | (FixtureVerdict::Unsafe, ScanVerdict::Unsafe { .. })
        )
    }
}

fn default_url() -> String {
    "https://example.com/red-team".to_string()
}

#[tokio::main]
async fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("examples/red-team/adversary-fixtures.json"));
    let fixtures = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    let fixtures: Vec<Fixture> = serde_json::from_str(&fixtures)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
    let scanner = AdversaryScanner::new(ScannerConfig::default());

    let mut failures = 0usize;
    for fixture in fixtures {
        let verdict = scanner
            .scan(&fixture.url, &fixture.content, fixture.context.into())
            .await;
        let expected = fixture.expected_for_local();
        let ok = expected.matches(&verdict);
        let actual = match &verdict {
            ScanVerdict::Clean => "clean".to_string(),
            ScanVerdict::Review { reason } => format!("review ({reason})"),
            ScanVerdict::Unsafe { reason } => format!("unsafe ({reason})"),
        };
        let marker = if ok { "ok" } else { "FAIL" };
        println!(
            "{marker}: {} layer={:?} expected_local={:?} actual={}",
            fixture.name, fixture.layer, expected, actual
        );
        if !ok {
            failures += 1;
        }
    }

    if failures > 0 {
        eprintln!("{failures} red-team fixture(s) failed");
        std::process::exit(1);
    }
}
