//! Statistical functions for verdict adjudication.
//!
//! Implements the distribution functions needed to turn test statistics into
//! p-values without pulling in a heavy statistics crate. The algorithms are
//! the standard series/continued-fraction expansions from Numerical Recipes
//! (3rd ed., §6.2–6.4).

// ---------------------------------------------------------------------------
// Incomplete gamma function  (chi-squared distribution)
// ---------------------------------------------------------------------------

const GAMMA_TINY: f64 = 3e-7;
const GAMMA_ITER: i32 = 200;

/// Lower regularized incomplete gamma P(a, x) = γ(a, x) / Γ(a).
///
/// Uses the series expansion for x < a+1 and the continued fraction for
/// x ≥ a+1. Returns 0.0 for x ≤ 0.
fn gammp(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x < a + 1.0 {
        gamma_series(a, x)
    } else {
        1.0 - gamma_cf(a, x)
    }
}

/// Upper regularized incomplete gamma Q(a, x) = 1 - P(a, x).
fn gammq(a: f64, x: f64) -> f64 {
    1.0 - gammp(a, x)
}

fn gamma_series(a: f64, x: f64) -> f64 {
    let ln_gamma = ln_gamma(a);
    let mut term = 1.0 / a;
    let mut sum = term;
    let mut n = 1;
    while n < GAMMA_ITER {
        term *= x / (a + n as f64);
        sum += term;
        if term.abs() < sum.abs() * GAMMA_TINY {
            break;
        }
        n += 1;
    }
    sum * (a * x.ln() - x - ln_gamma).exp()
}

fn gamma_cf(a: f64, x: f64) -> f64 {
    let ln_gamma = ln_gamma(a);
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / f64::MIN_POSITIVE;
    let mut d = 1.0 / b;
    let mut h = d;
    let mut n = 1;
    while n < GAMMA_ITER {
        let an = -(n as f64) * (n as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < f64::MIN_POSITIVE {
            d = f64::MIN_POSITIVE;
        }
        c = b + an / c;
        if c.abs() < f64::MIN_POSITIVE {
            c = f64::MIN_POSITIVE;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < GAMMA_TINY {
            break;
        }
        n += 1;
    }
    (a * x.ln() - x - ln_gamma).exp() * h
}

/// ln(Γ(x)) via the Lanczos approximation (Numerical Recipes §6.1).
fn ln_gamma(x: f64) -> f64 {
    const COF: [f64; 6] = [
        76.18009172947146,
        -86.50532032941677,
        24.01409824083091,
        -1.231739572450155,
        0.1208650973866179e-2,
        -0.5395239384953e-5,
    ];
    let y = x;
    let mut tmp = x + 5.5;
    tmp -= (x + 0.5) * tmp.ln();
    let mut ser = 1.000000000190015;
    for (j, c) in COF.iter().enumerate() {
        ser += c / (y + j as f64 + 1.0);
    }
    -tmp + ((2.0 * std::f64::consts::PI).sqrt() * ser / y).ln()
}

// ---------------------------------------------------------------------------
// Incomplete beta function  (t-distribution, F-distribution)
// ---------------------------------------------------------------------------

const BETA_TINY: f64 = 3e-7;
const BETA_ITER: i32 = 300;

/// Regularized incomplete beta function I_x(a, b).
///
/// Uses the continued fraction representation. Swaps symmetry when
/// x > (a+1)/(a+b+2) for numerical stability.
fn betai(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_beta = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b);
    let front = (a * x.ln() + b * (1.0 - x).ln() + ln_beta).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_cf(a, b, x) / a
    } else {
        1.0 - front * beta_cf(b, a, 1.0 - x) / b
    }
}

fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < f64::MIN_POSITIVE {
        d = f64::MIN_POSITIVE;
    }
    d = 1.0 / d;
    let mut h = d;
    let mut n = 1;
    while n <= BETA_ITER {
        let m = n as f64;
        let aa = m * (b - m) * x / ((qam + 2.0 * m) * (a + 2.0 * m));
        d = 1.0 + aa * d;
        if d.abs() < f64::MIN_POSITIVE {
            d = f64::MIN_POSITIVE;
        }
        c = 1.0 + aa / c;
        if c.abs() < f64::MIN_POSITIVE {
            c = f64::MIN_POSITIVE;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + 2.0 * m) * (qap + 2.0 * m));
        d = 1.0 + aa * d;
        if d.abs() < f64::MIN_POSITIVE {
            d = f64::MIN_POSITIVE;
        }
        c = 1.0 + aa / c;
        if c.abs() < f64::MIN_POSITIVE {
            c = f64::MIN_POSITIVE;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < BETA_TINY {
            break;
        }
        n += 1;
    }
    h
}

// ---------------------------------------------------------------------------
// Distribution survival functions  (1 - CDF)
// ---------------------------------------------------------------------------

/// Chi-squared survival function: P(X > x) for df degrees of freedom.
/// This is the p-value for a chi-squared test.
pub fn chi2_sf(x: f64, df: f64) -> f64 {
    if df <= 0.0 {
        return f64::NAN;
    }
    if x <= 0.0 {
        return 1.0;
    }
    gammq(df / 2.0, x / 2.0)
}

/// Two-tailed t-distribution survival function: P(|T| > |t|) for df degrees
/// of freedom. This is the two-sided p-value for a t-test.
pub fn t_sf_two_tailed(t: f64, df: f64) -> f64 {
    if df <= 0.0 {
        return f64::NAN;
    }
    if t == 0.0 {
        return 1.0;
    }
    let x = df / (df + t * t);
    let one_tail = 0.5 * betai(df / 2.0, 0.5, x);
    (2.0 * one_tail).min(1.0)
}

// ---------------------------------------------------------------------------
// Test wrappers
// ---------------------------------------------------------------------------

/// McNemar's test p-value for paired binary outcomes.
///
/// `b` = baseline-only successes (regressed), `c` = treatment-only successes
/// (improved). Uses the exact binomial test when b+c < 25 (discordant pairs
/// too few for the chi-squared approximation), otherwise the chi-squared
/// approximation with continuity correction.
pub fn mcnemar_pvalue(b: usize, c: usize) -> f64 {
    let discordant = b + c;
    if discordant == 0 {
        return 1.0;
    }
    if discordant < 25 {
        // Exact binomial: under H0, b ~ Binomial(discordant, 0.5).
        // Two-tailed p = 2 * min(P(X<=b), P(X>=b))
        let p_less_eq = binomial_cdf(b as i64, discordant, 0.5);
        let p_greater_eq = 1.0 - binomial_cdf(b as i64 - 1, discordant, 0.5);
        let one_tail = p_less_eq.min(p_greater_eq);
        return (2.0 * one_tail).min(1.0);
    }
    // Chi-squared with continuity correction, 1 df.
    let chi2 = ((b as f64 - c as f64).abs() - 1.0).powi(2) / discordant as f64;
    chi2_sf(chi2, 1.0)
}

/// Binomial CDF: P(X ≤ k) for X ~ Binomial(n, p).
fn binomial_cdf(k: i64, n: usize, p: f64) -> f64 {
    if k < 0 {
        return 0.0;
    }
    if k >= n as i64 {
        return 1.0;
    }
    // P(X ≤ k) = I_{1-p}(n-k, k+1)
    betai(n as f64 - k as f64, (k + 1) as f64, 1.0 - p)
}

/// Paired t-test result for a vector of paired deltas (treatment - baseline).
///
/// Returns (mean_delta, std_delta, n, t_statistic, p_value).
/// Returns None if there are fewer than 2 deltas (can't compute variance).
pub fn paired_t_test(deltas: &[f64]) -> Option<(f64, f64, usize, f64, f64)> {
    let n = deltas.len();
    if n < 2 {
        return None;
    }
    let mean = deltas.iter().sum::<f64>() / n as f64;
    let variance = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    let std = variance.sqrt();
    if std == 0.0 {
        // All deltas identical — no variance, result is exact.
        return if mean == 0.0 {
            Some((mean, std, n, 0.0, 1.0))
        } else {
            Some((mean, std, n, f64::INFINITY, 0.0))
        };
    }
    let t = mean / (std / (n as f64).sqrt());
    let p = t_sf_two_tailed(t, (n - 1) as f64);
    Some((mean, std, n, t, p))
}

/// Welch's t-test for two independent samples.
///
/// Returns (mean_a, mean_b, delta, t_statistic, df, p_value).
/// Returns None if either sample has fewer than 2 elements.
pub fn welch_t_test(a: &[f64], b: &[f64]) -> Option<(f64, f64, f64, f64, f64, f64)> {
    let na = a.len();
    let nb = b.len();
    if na < 2 || nb < 2 {
        return None;
    }
    let mean_a = a.iter().sum::<f64>() / na as f64;
    let mean_b = b.iter().sum::<f64>() / nb as f64;
    let var_a = a.iter().map(|x| (x - mean_a).powi(2)).sum::<f64>() / (na - 1) as f64;
    let var_b = b.iter().map(|x| (x - mean_b).powi(2)).sum::<f64>() / (nb - 1) as f64;
    let se_a = var_a / na as f64;
    let se_b = var_b / nb as f64;
    let se = (se_a + se_b).sqrt();
    if se == 0.0 {
        let delta = mean_b - mean_a;
        return if delta == 0.0 {
            Some((mean_a, mean_b, delta, 0.0, (na + nb - 2) as f64, 1.0))
        } else {
            Some((
                mean_a,
                mean_b,
                delta,
                f64::INFINITY,
                (na + nb - 2) as f64,
                0.0,
            ))
        };
    }
    let t = (mean_b - mean_a) / se;
    // Welch-Satterthwaite degrees of freedom.
    let df =
        (se_a + se_b).powi(2) / (se_a.powi(2) / (na - 1) as f64 + se_b.powi(2) / (nb - 1) as f64);
    let p = t_sf_two_tailed(t, df);
    Some((mean_a, mean_b, mean_b - mean_a, t, df, p))
}

/// Cohen's d effect size for paired samples: mean(δ) / sd(δ).
pub fn cohens_d_paired(deltas: &[f64]) -> Option<f64> {
    let n = deltas.len();
    if n < 2 {
        return None;
    }
    let mean = deltas.iter().sum::<f64>() / n as f64;
    let variance = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    let std = variance.sqrt();
    if std == 0.0 {
        return None;
    }
    Some(mean / std)
}

/// Cohen's h effect size for two proportions: 2*arcsin(sqrt(p2)) - 2*arcsin(sqrt(p1)).
pub fn cohens_h(p1: f64, p2: f64) -> f64 {
    2.0 * p2.sqrt().asin() - 2.0 * p1.sqrt().asin()
}

/// Label for a Cohen's d or h magnitude.
pub fn effect_label(d: f64) -> &'static str {
    let abs = d.abs();
    if abs < 0.2 {
        "negligible"
    } else if abs < 0.5 {
        "small"
    } else if abs < 0.8 {
        "medium"
    } else {
        "large"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chi2_sf_known_values() {
        // χ²(1) = 3.841 → p ≈ 0.05
        let p = chi2_sf(3.841, 1.0);
        assert!((p - 0.05).abs() < 0.001, "chi2_sf(3.841, 1) = {p}");
        // χ²(1) = 6.635 → p ≈ 0.01
        let p = chi2_sf(6.635, 1.0);
        assert!((p - 0.01).abs() < 0.001, "chi2_sf(6.635, 1) = {p}");
        // χ²(1) = 0 → p = 1.0
        assert_eq!(chi2_sf(0.0, 1.0), 1.0);
    }

    #[test]
    fn t_sf_two_tailed_known_values() {
        // t(10) = 2.228 → p ≈ 0.05
        let p = t_sf_two_tailed(2.228, 10.0);
        assert!((p - 0.05).abs() < 0.001, "t_sf(2.228, 10) = {p}");
        // t(∞) = 1.96 → p ≈ 0.05
        let p = t_sf_two_tailed(1.96, 10000.0);
        assert!((p - 0.05).abs() < 0.002, "t_sf(1.96, 10000) = {p}");
        // t = 0 → p = 1.0
        assert_eq!(t_sf_two_tailed(0.0, 10.0), 1.0);
    }

    #[test]
    fn mcnemar_exact_for_small_samples() {
        // b=0, c=10 → exact binomial, p should be very small
        let p = mcnemar_pvalue(0, 10);
        assert!(p < 0.01, "mcnemar(0, 10) = {p}");
        // b=5, c=5 → p should be 1.0
        let p = mcnemar_pvalue(5, 5);
        assert!((p - 1.0).abs() < 0.01, "mcnemar(5, 5) = {p}");
    }

    #[test]
    fn mcnemar_chi2_for_large_samples() {
        // b=30, c=10 → chi-squared approx
        let p = mcnemar_pvalue(30, 10);
        assert!(p < 0.01, "mcnemar(30, 10) = {p}");
        // b=20, c=20 → no evidence of difference, p should be high
        let p = mcnemar_pvalue(20, 20);
        assert!(p > 0.8, "mcnemar(20, 20) = {p}");
    }

    #[test]
    fn paired_t_test_basic() {
        // All zeros → t=0, p=1
        let (mean, _, n, t, p) = paired_t_test(&[0.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(mean, 0.0);
        assert_eq!(n, 4);
        assert_eq!(t, 0.0);
        assert_eq!(p, 1.0);

        // Consistent positive deltas → significant
        let (mean, _, n, _, p) = paired_t_test(&[1.0, 1.0, 1.0, 1.0, 1.0]).unwrap();
        assert_eq!(mean, 1.0);
        assert_eq!(n, 5);
        assert_eq!(p, 0.0); // zero variance, nonzero mean → p=0
    }

    #[test]
    fn paired_t_test_rejects_single_sample() {
        assert!(paired_t_test(&[1.0]).is_none());
        assert!(paired_t_test(&[]).is_none());
    }

    #[test]
    fn welch_t_test_basic() {
        // Same distributions → p should be high
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let b = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let (_, _, _, _, _, p) = welch_t_test(&a, &b).unwrap();
        assert!(p > 0.9, "welch same dist p = {p}");

        // Different means → p should be low
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let b = vec![11.0, 12.0, 13.0, 14.0, 15.0];
        let (_, _, delta, _, _, p) = welch_t_test(&a, &b).unwrap();
        assert_eq!(delta, 10.0);
        assert!(p < 0.01, "welch different means p = {p}");
    }

    #[test]
    fn cohens_h_known_values() {
        // p1=0.5, p2=0.5 → h=0
        let h = cohens_h(0.5, 0.5);
        assert!(h.abs() < 1e-10);
        // p1=0.1, p2=0.9 → h should be large
        let h = cohens_h(0.1, 0.9);
        assert!(h.abs() > 1.0);
    }

    #[test]
    fn effect_label_thresholds() {
        assert_eq!(effect_label(0.1), "negligible");
        assert_eq!(effect_label(0.3), "small");
        assert_eq!(effect_label(0.6), "medium");
        assert_eq!(effect_label(1.0), "large");
    }
}
