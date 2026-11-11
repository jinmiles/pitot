//! The Markdown report.

use std::fmt::Write;

use super::{Estimate, METRICS, Report, Scope, Verdict};
use crate::runner::BASELINE;

/// Significant digits that keep small values readable without inventing precision for large ones.
fn digits(value: f64) -> usize {
    match value.abs() {
        v if v == 0.0 || v >= 100.0 => 1,
        v if v >= 1.0 => 2,
        v if v >= 0.1 => 3,
        _ => 4,
    }
}

fn cell(e: Option<&Estimate>) -> String {
    let Some(e) = e else {
        return "n/a".to_owned();
    };
    let d = digits(e.mean.abs().max(e.sd));
    match (e.ci_low, e.ci_high) {
        (Some(low), Some(high)) => format!("{:.d$} ({:.d$} to {:.d$})", e.mean, low, high),
        _ => format!("{:.d$}", e.mean),
    }
}

fn verdict(v: Verdict) -> &'static str {
    match v {
        Verdict::Within => "within",
        Verdict::Over => "**over**",
        Verdict::Inconclusive => "inconclusive",
        Verdict::Missing => "no data",
    }
}

pub fn markdown(report: &Report) -> String {
    let mut out = String::new();
    let series = &report.series;
    let plan = &series.plan;
    let _ = writeln!(out, "# {} series {}\n", report.scenario, series.series_id);
    let machine = &report.machine;
    let cycle_rate = machine
        .cycle_rate_mhz
        .map(|rate| format!(", cycle counter {rate:.0} MHz"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "- Machine: {}, {} logical processors{cycle_rate}; {}",
        machine.cpu.as_deref().unwrap_or("unknown CPU"),
        machine.logical_processors,
        machine.os()
    );
    let _ = writeln!(
        out,
        "- Series: started {}, finished {}, {} runs, seed {}, plan `{}`",
        series.started_local,
        series.finished_local.as_deref().unwrap_or("not finished"),
        series.runs.len(),
        series.seed,
        series.plan_file
    );
    match &plan.replay {
        Some(replay) => {
            let file = replay.file().map_or("?", |(_, f)| f);
            let _ = writeln!(out, "- Replay: `{file}` at {}x, looped", replay.speed);
        }
        None => {
            let _ = writeln!(out, "- Replay: none (sim not running)");
        }
    }
    let _ = writeln!(
        out,
        "- Each run: {} s warm-up, {} s measurement at {} ms per sample, {} s cool-down",
        plan.warmup_s, plan.measure_s, plan.interval_ms, plan.cooldown_s
    );
    for app in &plan.apps {
        let mut line = format!("- App `{}`: `{}`", app.name, app.command);
        for arg in &app.args {
            let _ = write!(line, " `{arg}`");
        }
        if !app.measure.is_empty() {
            let _ = write!(line, ", measuring {}", app.measure.join(", "));
        }
        let _ = writeln!(out, "{line}");
    }
    if series.detected.is_empty() {
        let _ = writeln!(out, "- Known overlays running at the start: none");
    } else {
        let names: Vec<String> = series
            .detected
            .iter()
            .map(|f| format!("{} ({})", f.name, f.what))
            .collect();
        let _ = writeln!(
            out,
            "- **Known overlays running at the start: {}**",
            names.join(", ")
        );
    }

    let _ = writeln!(out, "\n## Results\n");
    let _ = writeln!(
        out,
        "Mean over the measured runs with its 95% confidence interval (Student's t). Runs measured: {}.\n",
        report
            .results
            .iter()
            .map(|r| format!("{} {} of {}", r.app, r.measured, r.runs))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut header = "| Metric | Unit |".to_owned();
    let mut rule = "|---|---|".to_owned();
    for result in &report.results {
        let _ = write!(header, " {} |", result.app);
        rule.push_str("---|");
    }
    let _ = writeln!(out, "{header}\n{rule}");
    for metric in METRICS {
        let mut row = format!("| {} | {} |", metric.label, metric.unit);
        for result in &report.results {
            // Baselines have no app, so its own values are zero by definition.
            let value = if result.app == BASELINE && metric.scope == Scope::App {
                "-".to_owned()
            } else {
                cell(result.metric(metric.key))
            };
            let _ = write!(row, " {value} |");
        }
        let _ = writeln!(out, "{row}");
    }

    let apps: Vec<_> = report.results.iter().skip(1).collect();
    if !apps.is_empty() {
        let _ = writeln!(out, "\n## Against the baselines\n");
        let _ = writeln!(
            out,
            "Desktop-wide values of each app's runs minus the interleaved baseline runs, with Welch's 95% interval \
(for an app with one run, from the baselines' spread). An interval that contains zero is no measurable difference.\n"
        );
        let mut header = "| Metric | Unit |".to_owned();
        let mut rule = "|---|---|".to_owned();
        for result in &apps {
            let _ = write!(header, " {} |", result.app);
            rule.push_str("---|");
        }
        let _ = writeln!(out, "{header}\n{rule}");
        for metric in METRICS.iter().filter(|m| m.scope == Scope::Desktop) {
            let mut row = format!("| {} | {} |", metric.label, metric.unit);
            for result in &apps {
                let _ = write!(row, " {} |", cell(result.difference(metric.key)));
            }
            let _ = writeln!(out, "{row}");
        }
    }

    if !report.budget.is_empty() {
        let _ = writeln!(out, "\n## Budget\n");
        let _ = writeln!(
            out,
            "Means are within when their whole interval is at or below the limit, over when it is entirely above, \
and inconclusive otherwise. Highs and peaks use the worst run.\n"
        );
        let _ = writeln!(
            out,
            "| App | Item | Limit | Value | Verdict |\n|---|---|---|---|---|"
        );
        for check in &report.budget {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                check.app,
                check.item,
                check.limit,
                check.value,
                verdict(check.verdict)
            );
        }
    }

    let _ = writeln!(out, "\n## Runs\n");
    let _ = writeln!(out, "| # | App | Outcome |\n|---|---|---|");
    for run in &report.runs {
        let _ = writeln!(out, "| {} | {} | {} |", run.position, run.app, run.status);
    }

    let ticks_only = report.results.iter().map(|r| r.tick_runs).sum::<usize>();
    let cpu_note = if ticks_only == 0 {
        "- CPU time is each process's CPU cycles at the cycle counter's rate, which the collector measures at the \
start of every run; the counter runs at a fixed rate whatever the clock speed. The OS's own count, in clock ticks \
of 15.6 ms by default, is shown for comparison: it rounds every sample to whole ticks.\n"
            .to_owned()
    } else {
        format!(
            "- CPU time is each process's CPU cycles at the cycle counter's rate, which the collector measures at the \
start of every run. {ticks_only} runs were collected without a measured rate; their CPU time is counted in clock ticks \
(15.6 ms by default) and their busiest sample is estimated from cycles at the run's average CPU time per cycle.\n"
        )
    };
    let _ = writeln!(out, "\n## Notes\n");
    let _ = writeln!(
        out,
        "- Process values are summed over each app's process tree. MB is 2^20 bytes.\n\
{cpu_note}\
- Private bytes growth is the least squares slope over each run; over short runs it reflects start-up and \
caching more than leaks.\n\
- DWM and the GPU belong to the whole desktop, so they are compared against the baselines. The collector's own \
CPU time is shown for reference.\n\
- Generated by {} {} from `{}`.",
        report.tool,
        report.version,
        crate::paths::series_file(&report.scenario, &series.series_id)
            .to_string_lossy()
            .replace('\\', "/")
    );
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::sample_report;
    use super::*;

    #[test]
    fn markdown_has_every_section() {
        let text = markdown(&sample_report());
        for heading in [
            "# unit series 20261111-000000",
            "## Results",
            "## Against the baselines",
            "## Budget",
            "## Runs",
            "## Notes",
        ] {
            assert!(text.contains(heading), "missing {heading}:\n{text}");
        }
        assert!(
            text.contains("| CPU, all cores | % | - | 0.0210 (0.0185 to 0.0235) |"),
            "{text}"
        );
        assert!(text.contains("| DWM CPU time | ms/s | 10.4"), "{text}");
        assert!(text.contains("| heavy | CPU, all cores, mean | 0.2 % |"));
        assert!(text.contains("**over**"));
        assert!(text.contains("Windows 11 Pro 25H2, build 26300.1"));
        assert!(text.contains("| 1 | baseline | measured |"));
        assert!(text.contains("not running after warm-up"));
    }

    #[test]
    fn digits_follow_the_magnitude() {
        assert_eq!(cell(Estimate::of(&[0.0212]).as_ref()), "0.0212");
        assert_eq!(cell(Estimate::of(&[14.12]).as_ref()), "14.12");
        assert_eq!(cell(Estimate::of(&[123.4]).as_ref()), "123.4");
        assert_eq!(cell(None), "n/a");
    }
}
