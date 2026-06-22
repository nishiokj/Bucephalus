CREATE OR REPLACE VIEW pass_rate_trend AS
SELECT
    run_id,
    variant_id,
    round(avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS pass_rate,
    count(*) AS n_trials
FROM trials
GROUP BY run_id, variant_id
ORDER BY run_id, variant_id;

CREATE OR REPLACE VIEW failure_clusters AS
SELECT
    CASE
        WHEN instr(task_id, '__') > 0 THEN substr(task_id, 1, instr(task_id, '__') - 1)
        ELSE task_id
    END AS task_group,
    count(*) AS total,
    sum(CASE WHEN outcome <> 'success' THEN 1 ELSE 0 END) AS failures,
    round(1.0 - avg(CASE WHEN outcome = 'success' THEN 1.0 ELSE 0.0 END), 4) AS failure_rate
FROM trials
GROUP BY task_group
ORDER BY failure_rate DESC, failures DESC, task_group;
