//! Statistics for reports: means with Student-t confidence intervals, Welch's difference of means (or Crawford
//! and Howell's for a single run against a sample), and least squares slopes. Platform independent and free of
//! dependencies.

use serde::{Deserialize, Serialize};

/// A mean over runs with its 95% confidence interval. The interval is missing with fewer than two values.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub n: usize,
    pub mean: f64,
    /// Sample standard deviation.
    pub sd: f64,
    pub ci_low: Option<f64>,
    pub ci_high: Option<f64>,
}

const CONFIDENCE: f64 = 0.95;

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn variance(values: &[f64], mean: f64) -> f64 {
    values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (values.len() - 1) as f64
}

impl Estimate {
    /// The mean of `values` with a t-based interval. `None` for no values.
    pub fn of(values: &[f64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let m = mean(values);
        if values.len() < 2 {
            return Some(Self {
                n: 1,
                mean: m,
                sd: 0.0,
                ci_low: None,
                ci_high: None,
            });
        }
        let sd = variance(values, m).sqrt();
        let half = t_quantile(0.5 + CONFIDENCE / 2.0, (values.len() - 1) as f64) * sd
            / (values.len() as f64).sqrt();
        Some(Self {
            n: values.len(),
            mean: m,
            sd,
            ci_low: Some(m - half),
            ci_high: Some(m + half),
        })
    }

    /// `a - b` with Welch's interval, which does not assume equal variances. When one side has a single value,
    /// such as the one app run of a 60-minute series, the interval comes from the other side's spread: Crawford
    /// and Howell's test of a single case against a sample, with standard error `s * sqrt((n + 1) / n)` and
    /// `n - 1` degrees of freedom. `None` if either side is empty or both have a single value.
    pub fn difference(a: &[f64], b: &[f64]) -> Option<Self> {
        if a.is_empty() || b.is_empty() || (a.len() < 2 && b.len() < 2) {
            return None;
        }
        let (ma, mb) = (mean(a), mean(b));
        let d = ma - mb;
        let (se, df) = if a.len() < 2 || b.len() < 2 {
            let (sample, m) = if a.len() < 2 { (b, mb) } else { (a, ma) };
            let n = sample.len() as f64;
            ((variance(sample, m) * (n + 1.0) / n).sqrt(), n - 1.0)
        } else {
            let (va, vb) = (
                variance(a, ma) / a.len() as f64,
                variance(b, mb) / b.len() as f64,
            );
            let df = (va + vb).powi(2)
                / (va.powi(2) / (a.len() - 1) as f64 + vb.powi(2) / (b.len() - 1) as f64);
            ((va + vb).sqrt(), df)
        };
        let half = if se == 0.0 {
            0.0
        } else {
            t_quantile(0.5 + CONFIDENCE / 2.0, df) * se
        };
        Some(Self {
            n: a.len().min(b.len()),
            mean: d,
            sd: se,
            ci_low: Some(d - half),
            ci_high: Some(d + half),
        })
    }
}

/// Least squares slope of `y` over `x`. `None` with fewer than two distinct `x` values.
pub fn slope(points: impl Iterator<Item = (f64, f64)> + Clone) -> Option<f64> {
    let (n, sx, sy) = points.clone().fold((0.0, 0.0, 0.0), |(n, sx, sy), (x, y)| {
        (n + 1.0, sx + x, sy + y)
    });
    if n < 2.0 {
        return None;
    }
    let (mx, my) = (sx / n, sy / n);
    let (sxy, sxx) = points.fold((0.0, 0.0), |(sxy, sxx), (x, y)| {
        (sxy + (x - mx) * (y - my), sxx + (x - mx).powi(2))
    });
    (sxx > 0.0).then(|| sxy / sxx)
}

/// The value below which a fraction `q` of `sorted` lies, interpolating between neighbors.
pub fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let pos = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    Some(sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64))
}

/// Natural log of the gamma function (Lanczos approximation, g = 7, n = 9), accurate to about 15 digits.
fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection formula.
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let t = x + G + 0.5;
    let sum = C[1..]
        .iter()
        .enumerate()
        .fold(C[0], |acc, (i, c)| acc + c / (x + i as f64 + 1.0));
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()
}

/// Continued fraction of the incomplete beta function, by the modified Lentz method.
fn beta_fraction(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    d = if d.abs() < TINY { TINY } else { d };
    d = 1.0 / d;
    let mut h = d;
    for m in 1..300 {
        let m = f64::from(m);
        let m2 = 2.0 * m;
        for aa in [
            m * (b - m) * x / ((qam + m2) * (a + m2)),
            -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2)),
        ] {
            d = 1.0 + aa * d;
            d = if d.abs() < TINY { TINY } else { d };
            c = 1.0 + aa / c;
            c = if c.abs() < TINY { TINY } else { c };
            d = 1.0 / d;
            h *= d * c;
        }
        if (d * c - 1.0).abs() < 1e-15 {
            break;
        }
    }
    h
}

/// Regularized incomplete beta function I_x(a, b).
fn incomplete_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    // The fraction converges quickly on this side of the mean; use the symmetry relation on the other.
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_fraction(a, b, x) / a
    } else {
        1.0 - front * beta_fraction(b, a, 1.0 - x) / b
    }
}

/// Cumulative distribution of Student's t with `df` degrees of freedom.
fn t_cdf(t: f64, df: f64) -> f64 {
    let tail = 0.5 * incomplete_beta(df / 2.0, 0.5, df / (df + t * t));
    if t >= 0.0 { 1.0 - tail } else { tail }
}

/// The `p` quantile of Student's t (`0.5 < p < 1`), by bisection on the distribution function.
pub fn t_quantile(p: f64, df: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 1.0e4);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if t_cdf(mid, df) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() <= tolerance
    }

    #[test]
    fn t_quantiles_match_tables() {
        for (df, expected) in [
            (1.0, 12.7062),
            (2.0, 4.3027),
            (4.0, 2.7764),
            (9.0, 2.2622),
            (30.0, 2.0423),
            (1.0e6, 1.9600),
        ] {
            let t = t_quantile(0.975, df);
            assert!(close(t, expected, 1e-3), "df {df}: {t} vs {expected}");
        }
        assert!(close(t_quantile(0.95, 5.0), 2.0150, 1e-3));
    }

    #[test]
    fn mean_with_interval() {
        let e = Estimate::of(&[1.0, 2.0, 3.0, 4.0, 5.0]).expect("values");
        assert_eq!((e.n, e.mean), (5, 3.0));
        assert!(close(e.sd, 1.5811, 1e-4));
        // 3 +- 2.7764 * 1.5811 / sqrt(5)
        assert!(close(e.ci_low.unwrap(), 1.0368, 1e-3));
        assert!(close(e.ci_high.unwrap(), 4.9632, 1e-3));
        let one = Estimate::of(&[7.0]).expect("one value");
        assert_eq!((one.mean, one.ci_low), (7.0, None));
        assert!(Estimate::of(&[]).is_none());
    }

    #[test]
    fn welch_difference() {
        let a = [20.1, 20.4, 19.8, 20.3, 20.0];
        let b = [19.0, 19.6, 18.7, 19.3, 19.9, 19.2];
        let d = Estimate::difference(&a, &b).expect("both have values");
        assert!(close(d.mean, 20.12 - 19.283_333, 1e-4));
        // Welch: standard error 0.20415 and 8.048 degrees of freedom, t = 2.30362 (checked by integrating the
        // t density numerically), so 0.83667 +- 0.47029.
        assert!(close(d.sd, 0.20415, 1e-5), "{d:?}");
        assert!(close(d.ci_low.unwrap(), 0.36638, 1e-4), "{d:?}");
        assert!(close(d.ci_high.unwrap(), 1.30695, 1e-4), "{d:?}");
        assert!(Estimate::difference(&[1.0], &[2.0]).is_none());
        assert!(Estimate::difference(&a, &[]).is_none());
    }

    #[test]
    fn single_run_against_a_sample() {
        // Crawford and Howell: 10 - 2 = 8, standard error sqrt(2) * sqrt(3 / 2) = 1.73205, one degree of freedom
        // (t = 12.7062), so 8 +- 22.0078.
        let d = Estimate::difference(&[10.0], &[1.0, 3.0]).expect("one run against two");
        assert_eq!((d.n, d.mean), (1, 8.0));
        assert!(close(d.sd, 1.73205, 1e-5), "{d:?}");
        assert!(close(d.ci_low.unwrap(), -14.0078, 1e-3), "{d:?}");
        assert!(close(d.ci_high.unwrap(), 30.0078, 1e-3), "{d:?}");
        let flipped = Estimate::difference(&[1.0, 3.0], &[10.0]).expect("two against one");
        assert_eq!(flipped.mean, -8.0);
        assert!(
            close(flipped.ci_high.unwrap(), 14.0078, 1e-3),
            "{flipped:?}"
        );
        // Five baselines with a small spread: a large single increase is clear.
        let clear = Estimate::difference(&[60.0], &[2.0, 2.5, 1.5, 3.0, 2.0])
            .expect("one run against five");
        assert!(clear.ci_low.unwrap() > 50.0, "{clear:?}");
    }

    #[test]
    fn slope_and_percentiles() {
        let line = [(0.0, 1.0), (1.0, 3.0), (2.0, 5.0), (3.0, 7.0)];
        assert_eq!(slope(line.iter().copied()), Some(2.0));
        assert_eq!(slope([(1.0, 1.0), (1.0, 2.0)].iter().copied()), None);
        let sorted = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&sorted, 0.5), Some(3.0));
        assert_eq!(percentile(&sorted, 1.0), Some(5.0));
        assert_eq!(percentile(&sorted, 0.9), Some(4.6));
        assert_eq!(percentile(&[], 0.5), None);
    }
}
