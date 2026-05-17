#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, relative, resolve } from 'node:path';
import { parseArgs } from 'node:util';

function loadJsonEnv(name, fallback) {
  const raw = process.env[name];
  if (!raw) return fallback;
  try {
    return JSON.parse(raw);
  } catch (err) {
    throw new Error(`Invalid JSON in ${name}: ${err instanceof Error ? err.message : String(err)}`);
  }
}

function loadAgentRuntimeCommand() {
  const fromJson = loadJsonEnv('AGENTLAB_AGENT_RUNTIME_CMD_JSON', null)
    ?? loadJsonEnv('AGENTLAB_HARNESS_CMD_JSON', null);
  if (Array.isArray(fromJson) && fromJson.length > 0 && fromJson.every((v) => typeof v === 'string')) {
    return fromJson;
  }

  const fromShell = process.env.AGENTLAB_AGENT_RUNTIME_CMD || process.env.AGENTLAB_HARNESS_CMD;
  if (fromShell && fromShell.trim().length > 0) {
    return fromShell.trim().split(/\s+/);
  }

  return ['python', '/opt/agent/harness.py', 'run'];
}

function loadPositiveInt(raw, fallback, label) {
  if (raw === undefined || raw === null || raw === '') return fallback;
  const parsed = Number.parseInt(String(raw), 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer; got "${raw}"`);
  }
  return parsed;
}

function countJsonlRecords(path) {
  const text = readFileSync(path, 'utf8');
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0).length;
}

function yamlScalar(value) {
  if (value === null) return 'null';
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return JSON.stringify(String(value));
}

function yaml(value, indent = 0) {
  const pad = ' '.repeat(indent);
  if (Array.isArray(value)) {
    if (value.length === 0) return '[]';
    return value
      .map((item) => {
        if (item && typeof item === 'object') {
          return `${pad}-\n${yaml(item, indent + 2)}`;
        }
        return `${pad}- ${yamlScalar(item)}`;
      })
      .join('\n');
  }
  if (value && typeof value === 'object') {
    const entries = Object.entries(value).filter(([, v]) => v !== undefined);
    if (entries.length === 0) return '{}';
    return entries
      .map(([key, item]) => {
        if (item && typeof item === 'object') {
          const rendered = yaml(item, indent + 2);
          return `${pad}${key}: ${rendered === '[]' || rendered === '{}' ? rendered : `\n${rendered}`}`;
        }
        return `${pad}${key}: ${yamlScalar(item)}`;
      })
      .join('\n');
  }
  return yamlScalar(value);
}

function buildExperiment({
  datasetPath,
  bundlePath,
  taskLimit,
  replications,
  randomSeed,
  maxConcurrency,
  agentRuntimeCommand,
  baselineBindings,
  treatmentBindings,
}) {
  return {
    experiment: {
      id: 'swebench_lite_curated_actual_agent_runtime',
      name: 'SWE-bench Lite Curated (Actual Agent Runtime)',
      workload_type: 'agent_runtime',
      description: 'Strict containerized eval over curated SWE-bench Lite with the real agent runtime.',
      owner: 'jevinnishioka',
    },
    dataset: {
      suite_id: 'swebench_lite_curated',
      provider: 'local_jsonl',
      path: datasetPath,
      split_id: 'test',
      limit: taskLimit,
    },
    design: {
      sanitization_profile: 'hermetic_functional',
      comparison: 'paired',
      replications,
      random_seed: randomSeed,
      shuffle_tasks: true,
      max_concurrency: maxConcurrency,
    },
    baseline: {
      variant_id: 'control',
      bindings: baselineBindings,
    },
    variant_plan: [
      {
        variant_id: 'treatment',
        bindings: treatmentBindings,
      },
    ],
    trial_runtime: {
      task: {
        interface: 'writable_workspace',
        workspace: {
          source: 'container_image',
          image: { from: 'task_row' },
          workdir: { from: 'task_row' },
        },
      },
      agent: {
        artifact: {
          source: bundlePath,
          mount: {
            path: '/opt/agent',
            read_only: true,
          },
        },
        command: agentRuntimeCommand,
        integration_level: 'cli_basic',
        network: 'full',
        outputs: {
          result: {
            capture: {
              type: 'file',
              path: '/agentlab/out/result.json',
              format: 'json',
            },
          },
        },
      },
      execution: {
        agent_site: 'task_runtime',
      },
      grader: {
        strategy: 'host',
        host: {
          capability: 'swebench_official',
        },
        command: [
          'python3',
          '__AGENTLAB_HOST_GRADER_CAPABILITY__/swebench_official/run_official_swebench_eval_from_agentlab.py',
          '--grader-input',
        ],
        conclusion: {
          mode: 'direct',
        },
      },
    },
    metrics: [
      {
        id: 'resolved',
        source: {
          type: 'grader_output',
          pointer: '/payload/resolved',
        },
        direction: 'maximize',
        primary: true,
        weight: 1,
      },
      {
        id: 'latency_ms',
        source: {
          type: 'agent_response',
          pointer: '/metrics/latency_ms',
        },
        direction: 'minimize',
        primary: false,
        weight: 0,
      },
    ],
    artifacts: {
      collect: ['artifacts/**', 'output/**', '**/*.patch'],
      diff: true,
    },
    policy: {
      timeout_ms: 600000,
      task_sandbox: {
        profile: 'hermetic_functional',
        network: 'full',
      },
    },
    validity: {
      fail_on_state_leak: true,
      fail_on_profile_invariant_violation: true,
    },
  };
}

function runLabJson(runnerBin, cwd, args) {
  const proc = spawnSync(runnerBin, args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  if (proc.status !== 0) {
    throw new Error(
      `lab command failed (${[runnerBin, ...args].join(' ')}):\n${proc.stderr || proc.stdout}`,
    );
  }
  const lastJsonLine = proc.stdout
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .at(-1);
  if (!lastJsonLine) {
    throw new Error('lab command produced no JSON output');
  }
  return JSON.parse(lastJsonLine);
}

function main() {
  const { values } = parseArgs({
    options: {
      dataset: { type: 'string', default: 'data/swebench_lite_curated.jsonl' },
      experiment: { type: 'string', default: '.lab/experiments/swebench_lite_curated.yaml' },
      'write-only': { type: 'boolean', default: false },
      limit: { type: 'string' },
      replications: { type: 'string', default: process.env.AGENTLAB_REPLICATIONS || '1' },
      seed: { type: 'string', default: process.env.AGENTLAB_RANDOM_SEED || '42' },
      concurrency: { type: 'string', default: process.env.AGENTLAB_MAX_CONCURRENCY || '1' },
      'runner-bin': { type: 'string', default: process.env.AGENTLAB_RUNNER_BIN || 'lab' },
    },
    allowPositionals: false,
  });

  const cwd = process.cwd();
  const datasetAbs = resolve(cwd, values.dataset);
  if (!existsSync(datasetAbs)) {
    throw new Error(
      `Dataset not found at ${values.dataset}. Generate it first with:\n` +
      '  node scripts/build-curated-swebench-lite.mjs',
    );
  }

  const expAbs = resolve(cwd, values.experiment);
  mkdirSync(dirname(expAbs), { recursive: true });

  const replications = loadPositiveInt(values.replications, 1, '--replications');
  const randomSeed = loadPositiveInt(values.seed, 42, '--seed');
  const maxConcurrency = loadPositiveInt(values.concurrency, 1, '--concurrency');
  const datasetCount = countJsonlRecords(datasetAbs);
  const limit = loadPositiveInt(values.limit, datasetCount, '--limit');
  const safeLimit = Math.min(limit, datasetCount);
  if (safeLimit <= 0) {
    throw new Error('Dataset is empty.');
  }

  const bundle = process.env.AGENTLAB_AGENT_BUNDLE || '.lab/agents/agent-runtime.tar.gz';
  const bundleAbs = resolve(cwd, bundle);
  if (!existsSync(bundleAbs)) {
    throw new Error(`Agent bundle not found at ${bundle}. Set AGENTLAB_AGENT_BUNDLE or build the runtime bundle first.`);
  }

  const expDirAbs = dirname(expAbs);
  const experiment = buildExperiment({
    datasetPath: relative(expDirAbs, datasetAbs),
    bundlePath: relative(expDirAbs, bundleAbs),
    taskLimit: safeLimit,
    replications,
    randomSeed,
    maxConcurrency,
    agentRuntimeCommand: loadAgentRuntimeCommand(),
    baselineBindings: loadJsonEnv('AGENTLAB_BASELINE_BINDINGS_JSON', {
      prompt_profile: 'baseline',
    }),
    treatmentBindings: loadJsonEnv('AGENTLAB_TREATMENT_BINDINGS_JSON', {
      prompt_profile: 'treatment',
    }),
  });

  writeFileSync(expAbs, `${yaml(experiment)}\n`);
  console.log(`Wrote experiment config: ${values.experiment}`);
  console.log(`Agent runtime command: ${JSON.stringify(experiment.trial_runtime.agent.command)}`);
  console.log(`Dataset tasks: ${datasetCount} (limit=${safeLimit})`);

  if (values['write-only']) {
    console.log('Write only; skipping run.');
    return;
  }

  console.log(`Planned trials: ${safeLimit * 2 * replications}`);
  const run = runLabJson(values['runner-bin'], cwd, ['run', values.experiment, '--json']);
  console.log(`Run complete: ${run.run?.run_id ?? '<unknown>'}`);
}

try {
  main();
} catch (err) {
  console.error(err instanceof Error ? err.message : String(err));
  process.exit(1);
}
