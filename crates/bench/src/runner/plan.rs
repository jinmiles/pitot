//! Measurement plans: what to run, how long, and in which order.

use std::fmt;

use serde::{Deserialize, Serialize};

/// App name of the runs without an overlay.
pub const BASELINE: &str = "baseline";

/// A measurement series read from a TOML file. See `docs/runner.md`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Output directory under `bench-results/`.
    pub scenario: String,
    /// Runs per app. Baselines run once more than that.
    #[serde(default = "defaults::repeats")]
    pub repeats: u32,
    #[serde(default = "defaults::warmup")]
    pub warmup_s: f64,
    #[serde(default = "defaults::measure")]
    pub measure_s: f64,
    #[serde(default = "defaults::cooldown")]
    pub cooldown_s: f64,
    #[serde(default = "defaults::interval")]
    pub interval_ms: u64,
    /// Seed for the run order; a random one is chosen and recorded when left out.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Recording replayed during every run. Without one, the apps run with the sim not running.
    #[serde(default)]
    pub replay: Option<ReplayPlan>,
    #[serde(default, rename = "app")]
    pub apps: Vec<AppPlan>,
    /// More executable names to warn about before the series, besides the built-in list.
    #[serde(default)]
    pub detect: Vec<String>,
    /// Limits the report checks every app against.
    #[serde(default)]
    pub budget: Option<Budget>,
}

/// Performance limits, each optional. The report marks every app within, over, or inconclusive for each.
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Mean CPU as a percentage of all logical processors.
    #[serde(default)]
    pub cpu_pct: Option<f64>,
    /// Highest one-sample CPU, in ms per second (10 is 1% of one core).
    #[serde(default)]
    pub cpu_peak_ms_per_s: Option<f64>,
    /// Highest private bytes, in MB (2^20 bytes).
    #[serde(default)]
    pub private_mb: Option<f64>,
    #[serde(default)]
    pub private_growth_mb_per_h: Option<f64>,
    /// DWM CPU and GPU must not rise measurably over the baselines.
    #[serde(default)]
    pub dwm_no_increase: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayPlan {
    #[serde(default)]
    pub irdt: Option<String>,
    #[serde(default)]
    pub ibt: Option<String>,
    /// A Pitot capture (`.ptc`).
    #[serde(default)]
    pub capture: Option<String>,
    #[serde(default = "defaults::speed")]
    pub speed: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppPlan {
    /// Output directory name for the app's runs.
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Executable names to measure, with their process trees. Left out, the launched process tree is measured.
    #[serde(default)]
    pub measure: Vec<String>,
    /// Files put in place before each run of this app and restored after it.
    #[serde(default)]
    pub files: Vec<FilePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilePlan {
    pub from: String,
    /// Destination; `%NAME%` expands to the environment variable `NAME`.
    pub to: String,
}

impl ReplayPlan {
    /// The recording and its option name, when exactly one is given.
    pub fn file(&self) -> Option<(&'static str, &str)> {
        let given: Vec<(&'static str, &str)> = [
            ("--irdt", self.irdt.as_deref()),
            ("--ibt", self.ibt.as_deref()),
            ("--capture", self.capture.as_deref()),
        ]
        .into_iter()
        .filter_map(|(option, file)| Some((option, file?)))
        .collect();
        match given[..] {
            [one] => Some(one),
            _ => None,
        }
    }
}

mod defaults {
    pub fn repeats() -> u32 {
        5
    }
    pub fn warmup() -> f64 {
        60.0
    }
    pub fn measure() -> f64 {
        180.0
    }
    pub fn cooldown() -> f64 {
        10.0
    }
    pub fn interval() -> u64 {
        1000
    }
    pub fn speed() -> f64 {
        1.0
    }
}

#[derive(Debug)]
pub enum PlanError {
    Toml(toml::de::Error),
    Invalid(String),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(e) => write!(f, "{e}"),
            Self::Invalid(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for PlanError {}

/// Names that become directory names: letters, digits, `.`, `-`, `_`, not starting with `.`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

impl Plan {
    pub fn parse(text: &str) -> Result<Self, PlanError> {
        let plan: Self = toml::from_str(text).map_err(PlanError::Toml)?;
        plan.validate().map_err(PlanError::Invalid)?;
        Ok(plan)
    }

    fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.scenario) {
            return Err(format!(
                "scenario `{}` must be a plain name (letters, digits, `.`, `-`, `_`)",
                self.scenario
            ));
        }
        if !(1..=100).contains(&self.repeats) {
            return Err("repeats must be 1 to 100".to_owned());
        }
        let seconds =
            |v: f64, min_exclusive: bool| v.is_finite() && (v > 0.0 || !min_exclusive && v == 0.0);
        if !seconds(self.warmup_s, false) || !seconds(self.cooldown_s, false) {
            return Err("warmup_s and cooldown_s must be 0 or more".to_owned());
        }
        if self.interval_ms < 100 {
            return Err("interval_ms must be at least 100".to_owned());
        }
        if !seconds(self.measure_s, true) || self.measure_s * 1000.0 < self.interval_ms as f64 {
            return Err("measure_s must cover at least one interval".to_owned());
        }
        if let Some(replay) = &self.replay {
            if replay.file().is_none() {
                return Err("[replay] needs exactly one of irdt, ibt, or capture".to_owned());
            }
            if !seconds(replay.speed, true) {
                return Err("[replay] speed must be positive".to_owned());
            }
        }
        if self.apps.is_empty() {
            return Err("the plan needs at least one [[app]]".to_owned());
        }
        for (i, app) in self.apps.iter().enumerate() {
            if !valid_name(&app.name) || app.name == BASELINE || app.name == crate::paths::SERIES {
                return Err(format!(
                    "app name `{}` must be a plain name other than `{BASELINE}` and `{}`",
                    app.name,
                    crate::paths::SERIES
                ));
            }
            if self.apps[..i].iter().any(|a| a.name == app.name) {
                return Err(format!("app name `{}` is used twice", app.name));
            }
            if app.command.is_empty() {
                return Err(format!("app `{}` needs a command", app.name));
            }
            if app
                .files
                .iter()
                .any(|f| f.from.is_empty() || f.to.is_empty())
            {
                return Err(format!("app `{}` has a file without from or to", app.name));
            }
        }
        Ok(())
    }

    pub fn app(&self, name: &str) -> Option<&AppPlan> {
        self.apps.iter().find(|a| a.name == name)
    }

    /// Wall time of the whole series, without process start and stop.
    pub fn estimated_seconds(&self) -> f64 {
        let runs = schedule_len(self.apps.len(), self.repeats) as f64;
        runs * (self.warmup_s + self.measure_s + self.cooldown_s)
    }
}

fn schedule_len(apps: usize, repeats: u32) -> usize {
    (apps + 1) * repeats as usize + 1
}

/// One run in the series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    /// 1-based position in the series.
    pub position: u32,
    /// An app name, or [`BASELINE`].
    pub app: String,
    /// 0-based repeat of this app.
    pub repeat: u32,
}

/// The run order: each repeat is a baseline followed by every app in a random order, and the series ends with a
/// baseline, so drift over the series affects apps and baselines alike.
pub fn order(apps: &[&str], repeats: u32, seed: u64) -> Vec<Slot> {
    let mut rng = SplitMix64(seed);
    let mut slots = Vec::with_capacity(schedule_len(apps.len(), repeats));
    let push = |slots: &mut Vec<Slot>, app: &str, repeat: u32| {
        let position = slots.len() as u32 + 1;
        slots.push(Slot {
            position,
            app: app.to_owned(),
            repeat,
        });
    };
    let mut shuffled = apps.to_vec();
    for repeat in 0..repeats {
        push(&mut slots, BASELINE, repeat);
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, rng.below(i as u64 + 1) as usize);
        }
        for app in &shuffled {
            push(&mut slots, app, repeat);
        }
    }
    push(&mut slots, BASELINE, repeats);
    slots
}

/// SplitMix64: small, fast, and good enough to shuffle a run order reproducibly from a recorded seed.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`n > 0`) by multiply and shift, without modulo bias worth noting for small `n`.
    fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next()) * u128::from(n)) >> 64) as u64
    }
}

/// Expands `%NAME%` to the value `var` gives for `NAME`. A `%` without a closing one is an error.
pub fn expand(text: &str, var: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            return Err(format!("unclosed `%` in `{text}`"));
        };
        let name = &after[..end];
        let value = var(name).ok_or_else(|| format!("environment variable `{name}` is not set"))?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"
scenario = "tier-a-inputs"
repeats = 3
warmup_s = 5
measure_s = 10
seed = 7

[budget]
cpu_pct = 0.2
private_mb = 30
dwm_no_increase = true

[replay]
irdt = "data/race.irdt"

[[app]]
name = "pitot"
command = "target/release/pitot.exe"
files = [{ from = "benchmarks/pitot.toml", to = "%APPDATA%/Pitot/config.toml" }]

[[app]]
name = "other"
command = "C:/Apps/Other/other.exe"
args = ["--quiet"]
measure = ["other.exe", "other-helper.exe"]
"#;

    #[test]
    fn parses_a_plan_with_defaults() {
        let plan = Plan::parse(PLAN).expect("valid plan");
        assert_eq!(plan.scenario, "tier-a-inputs");
        assert_eq!(
            (plan.repeats, plan.warmup_s, plan.measure_s),
            (3, 5.0, 10.0)
        );
        assert_eq!((plan.cooldown_s, plan.interval_ms), (10.0, 1000));
        assert_eq!(plan.replay.as_ref().map(|r| r.speed), Some(1.0));
        assert_eq!(plan.apps.len(), 2);
        assert_eq!(plan.app("other").map(|a| a.measure.len()), Some(2));
        assert_eq!(plan.estimated_seconds(), 10.0 * 25.0, "10 runs of 25 s");
        let budget = plan.budget.expect("budget");
        assert_eq!((budget.cpu_pct, budget.private_mb), (Some(0.2), Some(30.0)));
        assert_eq!(budget.cpu_peak_ms_per_s, None);
        assert!(budget.dwm_no_increase);
    }

    #[test]
    fn rejects_bad_plans() {
        let bad = |from: &str, to: &str| {
            let text = PLAN.replacen(from, to, 1);
            assert!(Plan::parse(&text).is_err(), "should reject `{to}`");
        };
        bad("name = \"other\"", "name = \"pitot\"");
        bad("name = \"other\"", "name = \"baseline\"");
        bad("name = \"other\"", "name = \"../up\"");
        bad("scenario = \"tier-a-inputs\"", "scenario = \"a/b\"");
        bad("repeats = 3", "repeats = 0");
        bad("measure_s = 10", "measure_s = 0.05");
        bad("irdt = \"data/race.irdt\"", "speed = 2.0");
        bad(
            "irdt = \"data/race.irdt\"",
            "irdt = \"data/race.irdt\"\ncapture = \"data/race.ptc\"",
        );
        bad("seed = 7", "seed = 7\ntypo = 1");
    }

    #[test]
    fn baselines_interleave_with_shuffled_apps() {
        let slots = order(&["a", "b", "c"], 4, 42);
        assert_eq!(slots.len(), 17);
        for (i, slot) in slots.iter().enumerate() {
            assert_eq!(slot.position as usize, i + 1);
        }
        let names: Vec<&str> = slots.iter().map(|s| s.app.as_str()).collect();
        for repeat in 0..4 {
            let block = &names[repeat * 4..repeat * 4 + 4];
            assert_eq!(block[0], BASELINE);
            let mut apps = block[1..].to_vec();
            apps.sort_unstable();
            assert_eq!(apps, ["a", "b", "c"], "every app once per repeat");
            assert!(
                slots[repeat * 4 + 1..repeat * 4 + 4]
                    .iter()
                    .all(|s| s.repeat == repeat as u32)
            );
        }
        assert_eq!(names[16], BASELINE);
        assert_eq!(
            order(&["a", "b", "c"], 4, 42),
            slots,
            "same seed, same order"
        );
        let orders: std::collections::HashSet<Vec<String>> = (0..20)
            .map(|seed| {
                order(&["a", "b", "c"], 4, seed)
                    .into_iter()
                    .map(|s| s.app)
                    .collect()
            })
            .collect();
        assert!(orders.len() > 10, "seeds change the order");
    }

    #[test]
    fn expands_environment_variables() {
        let var =
            |name: &str| (name == "APPDATA").then(|| r"C:\Users\me\AppData\Roaming".to_owned());
        assert_eq!(
            expand("%APPDATA%/Pitot/config.toml", var),
            Ok(r"C:\Users\me\AppData\Roaming/Pitot/config.toml".to_owned())
        );
        assert_eq!(expand("plain/path", var), Ok("plain/path".to_owned()));
        assert!(expand("%MISSING%/x", var).is_err());
        assert!(expand("50%", var).is_err());
    }
}
