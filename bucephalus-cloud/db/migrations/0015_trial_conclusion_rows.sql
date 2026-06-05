DO $$
BEGIN
  IF to_regclass('bucephalus_runtime.benchmark_conclusion_rows') IS NOT NULL THEN
    IF to_regclass('bucephalus_runtime.trial_conclusion_rows') IS NULL THEN
      ALTER TABLE bucephalus_runtime.benchmark_conclusion_rows RENAME TO trial_conclusion_rows;
    ELSE
      INSERT INTO bucephalus_runtime.trial_conclusion_rows (
        account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json
      )
      SELECT account_id, run_id, schedule_idx, attempt, row_seq, slot_commit_id, row_json
      FROM bucephalus_runtime.benchmark_conclusion_rows
      ON CONFLICT (account_id, run_id, schedule_idx, attempt, row_seq) DO NOTHING;

      DROP TABLE bucephalus_runtime.benchmark_conclusion_rows;
    END IF;
  END IF;
END $$;
