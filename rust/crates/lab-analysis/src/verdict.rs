//! The verdict engine: turns ledger facts into a defensible adjudication.
//!
//! This is NOT a revival of the old view-dumping surface. The old surface
//! handed the user a pile of statistics and said "you decide." This engine
//! produces a single verdict per metric — HELD, REGRESSED, IMPROVED, or
//! UNDERPOWERED — with the statistical backing to defend it.
//!
//! The engine reads from the same committed-fact views the old surface used,
//! but adjudicates instead of displaying. Direction (maximize/minimize) is
//! applied so "REGRESSED" always means "got worse." Grader and environment
//! pinning are checked so attribution is possible.

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::Serialize;

use crate::stats;

// ---------------------------------------------------------------------------
// Public data model
// ---------------------------------------------------------------------------

/// The top-level verdict for a run.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub run_id: String,
    pub view_set: String,
    pub baseline_variant: String,
    pub treatment_variants: Vec<String>,
    pub metric_verdicts: Vec<MetricVerdict>,
    pub grader_pinned: bool,
    pub grader_digest: Option<String>,
    pub grader_digests_seen: Vec<String>,
    pub flaky_task_count: usize,
    pub total_trials: usize,
    pub paired_trials: usize,
}

/// Per-metric adjudication.
#[derive(Debug, Clone, Serialize)]
pub struct MetricVerdict {
    pub metric_name: String,
    pub semantic_key: Option<String>,
    pub direction: MetricDirection,
    pub baseline_mean: f64,
    pub treatment_mean: f64,
    pub delta: f64,
    pub delta_pct: Option<f64>,
    pub p_value: Option<f64>,
    pub effect_size: Option<f64>,
    pub effect_label: Option<String>,
    pub n_baseline: usize,
    pub n_treatment: usize,
    pub n_paired: usize,
    pub classification: Classification,
    pub moved_cases: Vec<MovedCase>,
}

/// What happened to this metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Classification {
    /// No significant change — within noise.
    Held,
    /// Significant change in the bad direction.
    Regressed,
    /// Significant change in the good direction.
    Improved,
    /// Not enough data to make a confident call.
    Underpowered,
}

/// Direction of "better" for a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MetricDirection {
    Maximize,
    Minimize,
    Unknown,
}

/// A single case (task × repl) that moved between variants.
#[derive(Debug, Clone, Serialize)]
pub struct MovedCase {
    pub task_id: String,
    pub repl_idx: i64,
    pub baseline_value: Option<f64>,
    pub treatment_value: Option<f64>,
    pub delta: Option<f64>,
    pub baseline_trial_id: String,
    pub treatment_trial_id: String,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ALPHA: f64 = 0.05;
/// Minimum paired observations for a confident "held" call. Below this, a
/// non-significant result is labeled UNDERPOWERED rather than HELD.
const MIN_PAIRED_FOR_HELD: usize = 20;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Compute the verdict for a run, comparing its baseline variant against
/// treatment variant(s).
///
/// For single-variant runs (no treatment), returns a verdict with no metric
/// comparisons — just pinning status and run summary. Cross-run comparison
/// (`--against <baseline-run>`) is a future extension.
pub fn compute_verdict(run_dir: &std::path::Path) -> Result<Verdict> {
    let context = crate::load_run_context(run_dir)?;
    if context.view_set == crate::ViewSet::CoreOnly {
        return Err(anyhow!(
            "this run has no comparison design (comparison=none or unknown); \
             the verdict engine needs at least a baseline and treatment variant"
        ));
    }

    let conn = crate::open_account_db(&context.db_path)?;
    crate::register_views(&conn, &context)?;

    let (baseline_variant, treatment_variants) = resolve_variants(&conn)?;

    if treatment_variants.is_empty() {
        return Ok(Verdict {
            run_id: context.run_id.clone(),
            view_set: context.view_set.as_str().to_string(),
            baseline_variant,
            treatment_variants: Vec::new(),
            metric_verdicts: Vec::new(),
            grader_pinned: true,
            grader_digest: None,
            grader_digests_seen: Vec::new(),
            flaky_task_count: 0,
            total_trials: 0,
            paired_trials: 0,
        });
    }

    let metric_defs = load_metric_definitions(&conn)?;
    let grader_info = check_grader_pinning(&conn)?;
    let flaky_count = count_flaky_tasks(&conn)?;
    let (total_trials, paired_trials) =
        count_trials(&conn, &baseline_variant, &treatment_variants)?;

    let mut metric_verdicts = Vec::new();

    // Primary outcome (binary pass/fail) — always adjudicated first.
    let outcome_verdict =
        adjudicate_primary_outcome(&conn, &baseline_variant, &treatment_variants)?;
    if let Some(v) = outcome_verdict {
        metric_verdicts.push(v);
    }

    // Continuous metrics from metric definitions.
    for def in &metric_defs {
        if def.id == "pass_rate" || def.id == "success" {
            continue; // already handled as primary outcome
        }
        if let Some(v) =
            adjudicate_continuous_metric(&conn, def, &baseline_variant, &treatment_variants)?
        {
            metric_verdicts.push(v);
        }
    }

    Ok(Verdict {
        run_id: context.run_id.clone(),
        view_set: context.view_set.as_str().to_string(),
        baseline_variant,
        treatment_variants,
        metric_verdicts,
        grader_pinned: grader_info.0,
        grader_digest: grader_info.1,
        grader_digests_seen: grader_info.2,
        flaky_task_count: flaky_count,
        total_trials,
        paired_trials,
    })
}

// ---------------------------------------------------------------------------
// Variant resolution
// ---------------------------------------------------------------------------

fn resolve_variants(conn: &Connection) -> Result<(String, Vec<String>)> {
    let mut stmt = conn.prepare("SELECT DISTINCT variant_id FROM trials ORDER BY variant_id")?;
    let variants: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    if variants.is_empty() {
        return Err(anyhow!("no trials found for this run"));
    }

    // Try ab_variant_roles first (handles multi-variant A/B correctly).
    let baseline = conn
        .query_row(
            "SELECT baseline_id FROM ab_variant_roles LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .unwrap_or_else(|| variants.first().cloned().unwrap_or_default());

    let treatment: Vec<String> = variants.into_iter().filter(|v| v != &baseline).collect();

    Ok((baseline, treatment))
}

// ---------------------------------------------------------------------------
// Metric definitions
// ---------------------------------------------------------------------------

struct MetricDef {
    id: String,
    semantic_key: Option<String>,
    direction: MetricDirection,
}

fn load_metric_definitions(conn: &Connection) -> Result<Vec<MetricDef>> {
    let mut stmt = conn.prepare(
        "SELECT metric_id, semantic_key, direction FROM metric_definitions ORDER BY metric_id",
    )?;
    let defs = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let semantic_key: Option<String> = row.get(1)?;
            let direction_str: Option<String> = row.get(2)?;
            let direction = match direction_str.as_deref() {
                Some("maximize") => MetricDirection::Maximize,
                Some("minimize") => MetricDirection::Minimize,
                _ => MetricDirection::Unknown,
            };
            Ok(MetricDef {
                id,
                semantic_key,
                direction,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(defs)
}

// ---------------------------------------------------------------------------
// Grader pinning
// ---------------------------------------------------------------------------

fn check_grader_pinning(conn: &Connection) -> Result<(bool, Option<String>, Vec<String>)> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT grader_digest FROM trial_grader_identity WHERE grader_digest IS NOT NULL",
    )?;
    let digests: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    if digests.is_empty() {
        // No grader digests found — grader identity not pinned (old data).
        return Ok((false, None, Vec::new()));
    }
    let pinned = digests.len() == 1;
    let primary = digests.first().cloned();
    Ok((pinned, primary, digests))
}

// ---------------------------------------------------------------------------
// Flaky task count
// ---------------------------------------------------------------------------

fn count_flaky_tasks(conn: &Connection) -> Result<usize> {
    let count: i64 = conn
        .query_row("SELECT count(*) FROM flaky_tasks", [], |row| row.get(0))
        .unwrap_or(0);
    Ok(count as usize)
}

// ---------------------------------------------------------------------------
// Trial counts
// ---------------------------------------------------------------------------

fn count_trials(
    conn: &Connection,
    baseline: &str,
    treatments: &[String],
) -> Result<(usize, usize)> {
    let baseline_literal = crate::sql_literal(baseline);
    let treatment_list = treatments
        .iter()
        .map(|t| crate::sql_literal(t))
        .collect::<Vec<_>>()
        .join(", ");

    let total: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM trials WHERE variant_id IN ({}, {})",
                baseline_literal, treatment_list
            ),
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let paired: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM paired_outcomes"),
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    Ok((total as usize, paired as usize))
}

// ---------------------------------------------------------------------------
// Primary outcome adjudication (binary pass/fail)
// ---------------------------------------------------------------------------

fn adjudicate_primary_outcome(
    conn: &Connection,
    baseline: &str,
    treatments: &[String],
) -> Result<Option<MetricVerdict>> {
    // Use the first treatment variant for the comparison.
    let treatment = treatments.first().unwrap();

    // Get McNemar contingency from the view.
    let mut stmt = conn.prepare(
        "SELECT
            count(*) FILTER (WHERE baseline_outcome = 'success' AND treatment_outcome <> 'success') AS base_only,
            count(*) FILTER (WHERE baseline_outcome <> 'success' AND treatment_outcome = 'success') AS treat_only
         FROM paired_outcomes",
    )?;
    let (base_only, treat_only): (i64, i64) = stmt
        .query_row([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap_or((0, 0));

    let n_discordant = (base_only + treat_only) as usize;
    if n_discordant == 0 {
        // No discordant pairs — outcome is identical between variants.
        // Still report it as HELD.
    }

    let p_value = stats::mcnemar_pvalue(base_only as usize, treat_only as usize);

    // Get pass rates.
    let baseline_rate = get_pass_rate(conn, baseline)?;
    let treatment_rate = get_pass_rate(conn, treatment)?;
    let delta = treatment_rate - baseline_rate;
    let delta_pct = if baseline_rate > 0.0 {
        Some((delta / baseline_rate) * 100.0)
    } else {
        None
    };

    let h = stats::cohens_h(baseline_rate, treatment_rate);
    let effect_lbl = stats::effect_label(h);

    let n_baseline = count_trials_for_variant(conn, baseline);
    let n_treatment = count_trials_for_variant(conn, treatment);
    let n_paired = count_paired_outcomes(conn);

    let direction = MetricDirection::Maximize; // pass_rate: higher is better
    let classification = classify(p_value, delta, direction, n_paired);

    let moved_cases = get_moved_outcome_cases(conn)?;

    Ok(Some(MetricVerdict {
        metric_name: "pass_rate".to_string(),
        semantic_key: Some("capability".to_string()),
        direction,
        baseline_mean: baseline_rate,
        treatment_mean: treatment_rate,
        delta,
        delta_pct,
        p_value: Some(p_value),
        effect_size: Some(h),
        effect_label: Some(effect_lbl.to_string()),
        n_baseline,
        n_treatment,
        n_paired,
        classification,
        moved_cases,
    }))
}

fn get_pass_rate(conn: &Connection, variant: &str) -> Result<f64> {
    let rate: Option<f64> = conn
        .query_row(
            &format!(
                "SELECT avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END)
                 FROM trials WHERE variant_id = {}",
                crate::sql_literal(variant)
            ),
            [],
            |row| row.get(0),
        )
        .ok();
    Ok(rate.unwrap_or(0.0))
}

fn count_trials_for_variant(conn: &Connection, variant: &str) -> usize {
    let count: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM trials WHERE variant_id = {}",
                crate::sql_literal(variant)
            ),
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    count as usize
}

fn count_paired_outcomes(conn: &Connection) -> usize {
    let count: i64 = conn
        .query_row("SELECT count(*) FROM paired_outcomes", [], |row| row.get(0))
        .unwrap_or(0);
    count as usize
}

fn get_moved_outcome_cases(conn: &Connection) -> Result<Vec<MovedCase>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, repl_idx,
                baseline_trial_id, treatment_trial_id,
                baseline_metric, treatment_metric, metric_delta
         FROM paired_outcomes
         WHERE delta_type IN ('regression', 'improvement')
         ORDER BY
            CASE delta_type WHEN 'regression' THEN 0 WHEN 'improvement' THEN 1 END,
            abs(metric_delta) DESC NULLS LAST
         LIMIT 20",
    )?;
    let cases = stmt
        .query_map([], |row| {
            let task_id: String = row.get(0)?;
            let repl_idx: i64 = row.get(1)?;
            let baseline_trial_id: String = row.get(2)?;
            let treatment_trial_id: String = row.get(3)?;
            let baseline_value: Option<f64> = row.get::<_, Option<f64>>(4).ok().flatten();
            let treatment_value: Option<f64> = row.get::<_, Option<f64>>(5).ok().flatten();
            let delta: Option<f64> = row.get::<_, Option<f64>>(6).ok().flatten();
            Ok(MovedCase {
                task_id,
                repl_idx,
                baseline_value,
                treatment_value,
                delta,
                baseline_trial_id,
                treatment_trial_id,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(cases)
}

// ---------------------------------------------------------------------------
// Continuous metric adjudication
// ---------------------------------------------------------------------------

fn adjudicate_continuous_metric(
    conn: &Connection,
    def: &MetricDef,
    baseline: &str,
    treatments: &[String],
) -> Result<Option<MetricVerdict>> {
    let treatment = treatments.first().unwrap();
    let baseline_lit = crate::sql_literal(baseline);
    let treatment_lit = crate::sql_literal(treatment);

    // Try paired comparison first (same task × repl).
    let paired_sql = format!(
        "SELECT
            b.task_id,
            b.repl_idx,
            b.trial_id,
            t.trial_id,
            CAST(b.metric_value AS DOUBLE) AS baseline_val,
            CAST(t.metric_value AS DOUBLE) AS treatment_val
         FROM metrics_long b
         JOIN metrics_long t
           ON t.task_id = b.task_id
          AND t.repl_idx = b.repl_idx
          AND t.metric_name = b.metric_name
          AND t.variant_id = {treatment_lit}
         WHERE b.metric_name = {metric_lit}
           AND b.variant_id = {baseline_lit}
           AND CAST(b.metric_value AS DOUBLE) IS NOT NULL
           AND CAST(t.metric_value AS DOUBLE) IS NOT NULL",
        baseline_lit = baseline_lit,
        treatment_lit = treatment_lit,
        metric_lit = crate::sql_literal(&def.id),
    );

    let mut stmt = conn.prepare(&paired_sql)?;
    let paired_rows: Vec<(String, i64, String, String, f64, f64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if paired_rows.is_empty() {
        // No paired data for this metric — try unpaired.
        return adjudicate_unpaired_metric(conn, def, baseline, treatment);
    }

    let deltas: Vec<f64> = paired_rows.iter().map(|r| r.5 - r.4).collect();

    let n_paired = deltas.len();
    let baseline_vals: Vec<f64> = paired_rows.iter().map(|r| r.4).collect();
    let treatment_vals: Vec<f64> = paired_rows.iter().map(|r| r.5).collect();

    let baseline_mean = baseline_vals.iter().sum::<f64>() / n_paired as f64;
    let treatment_mean = treatment_vals.iter().sum::<f64>() / n_paired as f64;
    let delta = treatment_mean - baseline_mean;
    let delta_pct = if baseline_mean.abs() > 1e-12 {
        Some((delta / baseline_mean.abs()) * 100.0)
    } else {
        None
    };

    let test_result = stats::paired_t_test(&deltas);
    let (p_value, effect_size) = match test_result {
        Some((_, _, _, _, p)) => {
            let d = stats::cohens_d_paired(&deltas);
            (Some(p), d)
        }
        None => (None, None),
    };

    let effect_lbl = effect_size.map(|e| stats::effect_label(e).to_string());

    let classification = match p_value {
        Some(p) => classify(p, delta, def.direction, n_paired),
        None => Classification::Underpowered,
    };

    // Collect moved cases (top 20 by |delta|).
    let mut moved: Vec<MovedCase> = paired_rows
        .iter()
        .map(|r| MovedCase {
            task_id: r.0.clone(),
            repl_idx: r.1,
            baseline_value: Some(r.4),
            treatment_value: Some(r.5),
            delta: Some(r.5 - r.4),
            baseline_trial_id: r.2.clone(),
            treatment_trial_id: r.3.clone(),
        })
        .collect();
    moved.sort_by(|a, b| {
        let da = a.delta.unwrap_or(0.0).abs();
        let db = b.delta.unwrap_or(0.0).abs();
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });
    moved.truncate(20);

    Ok(Some(MetricVerdict {
        metric_name: def.id.clone(),
        semantic_key: def.semantic_key.clone(),
        direction: def.direction,
        baseline_mean,
        treatment_mean,
        delta,
        delta_pct,
        p_value,
        effect_size,
        effect_label: effect_lbl,
        n_baseline: baseline_vals.len(),
        n_treatment: treatment_vals.len(),
        n_paired,
        classification,
        moved_cases: moved,
    }))
}

fn adjudicate_unpaired_metric(
    conn: &Connection,
    def: &MetricDef,
    baseline: &str,
    treatment: &str,
) -> Result<Option<MetricVerdict>> {
    let baseline_lit = crate::sql_literal(baseline);
    let treatment_lit = crate::sql_literal(treatment);
    let metric_lit = crate::sql_literal(&def.id);

    let sql = format!(
        "SELECT variant_id, CAST(metric_value AS DOUBLE) AS val
         FROM metrics_long
         WHERE metric_name = {metric_lit}
           AND variant_id IN ({baseline_lit}, {treatment_lit})
           AND CAST(metric_value AS DOUBLE) IS NOT NULL",
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, f64)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let baseline_vals: Vec<f64> = rows
        .iter()
        .filter(|r| r.0 == baseline)
        .map(|r| r.1)
        .collect();
    let treatment_vals: Vec<f64> = rows
        .iter()
        .filter(|r| r.0 == treatment)
        .map(|r| r.1)
        .collect();

    if baseline_vals.is_empty() || treatment_vals.is_empty() {
        return Ok(None);
    }

    let (p_value, effect_size, baseline_mean, treatment_mean, delta) =
        match stats::welch_t_test(&baseline_vals, &treatment_vals) {
            Some((mean_a, mean_b, d, _, _, p)) => {
                let pooled_std = {
                    let var_a = if baseline_vals.len() > 1 {
                        baseline_vals
                            .iter()
                            .map(|x| (x - mean_a).powi(2))
                            .sum::<f64>()
                            / (baseline_vals.len() - 1) as f64
                    } else {
                        0.0
                    };
                    let var_b = if treatment_vals.len() > 1 {
                        treatment_vals
                            .iter()
                            .map(|x| (x - mean_b).powi(2))
                            .sum::<f64>()
                            / (treatment_vals.len() - 1) as f64
                    } else {
                        0.0
                    };
                    ((var_a + var_b) / 2.0).sqrt()
                };
                let cohens_d = if pooled_std > 0.0 {
                    Some(d / pooled_std)
                } else {
                    None
                };
                (Some(p), cohens_d, mean_a, mean_b, d)
            }
            None => (None, None, 0.0, 0.0, 0.0),
        };

    let delta_pct = if baseline_mean.abs() > 1e-12 {
        Some((delta / baseline_mean.abs()) * 100.0)
    } else {
        None
    };

    let n_total = baseline_vals.len() + treatment_vals.len();
    let effect_lbl = effect_size.map(|e| stats::effect_label(e).to_string());

    let classification = match p_value {
        Some(p) => classify(p, delta, def.direction, n_total),
        None => Classification::Underpowered,
    };

    Ok(Some(MetricVerdict {
        metric_name: def.id.clone(),
        semantic_key: def.semantic_key.clone(),
        direction: def.direction,
        baseline_mean,
        treatment_mean,
        delta,
        delta_pct,
        p_value,
        effect_size,
        effect_label: effect_lbl,
        n_baseline: baseline_vals.len(),
        n_treatment: treatment_vals.len(),
        n_paired: 0,
        classification,
        moved_cases: Vec::new(),
    }))
}

// ---------------------------------------------------------------------------
// Classification logic
// ---------------------------------------------------------------------------

/// Classify a metric change into HELD / REGRESSED / IMPROVED / UNDERPOWERED.
///
/// - If p < ALPHA: the change is real. Direction determines REGRESSED vs
///   IMPROVED. If direction is Unknown, we label based on the sign of delta
///   but mark it as "changed" rather than good/bad.
/// - If p ≥ ALPHA and sample size is adequate (≥ MIN_PAIRED_FOR_HELD): HELD.
/// - If p ≥ ALPHA and sample size is too small: UNDERPOWERED.
fn classify(p_value: f64, delta: f64, direction: MetricDirection, n: usize) -> Classification {
    if p_value < ALPHA {
        // Significant change — is it good or bad?
        match direction {
            MetricDirection::Maximize => {
                if delta > 0.0 {
                    Classification::Improved
                } else {
                    Classification::Regressed
                }
            }
            MetricDirection::Minimize => {
                if delta < 0.0 {
                    Classification::Improved
                } else {
                    Classification::Regressed
                }
            }
            MetricDirection::Unknown => {
                // No direction declared — can't say if it's good or bad.
                // Treat any significant change as a regression (conservative).
                if delta != 0.0 {
                    Classification::Regressed
                } else {
                    Classification::Held
                }
            }
        }
    } else if n >= MIN_PAIRED_FOR_HELD {
        Classification::Held
    } else {
        Classification::Underpowered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_held_when_not_significant_and_adequate_sample() {
        let c = classify(0.5, 0.01, MetricDirection::Maximize, 100);
        assert_eq!(c, Classification::Held);
    }

    #[test]
    fn classify_underpowered_when_not_significant_and_small_sample() {
        let c = classify(0.5, 0.01, MetricDirection::Maximize, 5);
        assert_eq!(c, Classification::Underpowered);
    }

    #[test]
    fn classify_regressed_when_significant_and_delta_is_bad() {
        // Maximize: delta < 0 is bad
        let c = classify(0.01, -0.1, MetricDirection::Maximize, 100);
        assert_eq!(c, Classification::Regressed);

        // Minimize: delta > 0 is bad
        let c = classify(0.01, 0.1, MetricDirection::Minimize, 100);
        assert_eq!(c, Classification::Regressed);
    }

    #[test]
    fn classify_improved_when_significant_and_delta_is_good() {
        // Maximize: delta > 0 is good
        let c = classify(0.01, 0.1, MetricDirection::Maximize, 100);
        assert_eq!(c, Classification::Improved);

        // Minimize: delta < 0 is good
        let c = classify(0.01, -0.1, MetricDirection::Minimize, 100);
        assert_eq!(c, Classification::Improved);
    }

    #[test]
    fn classify_unknown_direction_is_conservative() {
        // Significant change with unknown direction → Regressed (conservative)
        let c = classify(0.01, 0.1, MetricDirection::Unknown, 100);
        assert_eq!(c, Classification::Regressed);

        // No change → Held
        let c = classify(0.01, 0.0, MetricDirection::Unknown, 100);
        assert_eq!(c, Classification::Held);
    }

    #[test]
    fn classify_boundary_alpha() {
        // p exactly at alpha → not significant
        let c = classify(ALPHA, 0.1, MetricDirection::Maximize, 100);
        assert_eq!(c, Classification::Held);
    }
}
