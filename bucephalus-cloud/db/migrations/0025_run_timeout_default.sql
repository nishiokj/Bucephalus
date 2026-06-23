-- Run requirements now always carry an explicit timeout_ms. Backfill any
-- existing runs that were created without one (null or missing key) with the
-- same 15-minute default the worker previously smuggled in implicitly.
UPDATE cloud.runs
  SET run_requirements = jsonb_set(
    run_requirements,
    '{timeout_ms}',
    to_jsonb(900000)
  )
  WHERE run_requirements->>'timeout_ms' IS NULL
     OR run_requirements->>'timeout_ms' !~ '^[1-9][0-9]*$';
