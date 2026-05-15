#!/usr/bin/env node
const fs = require('fs');

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function writeJson(filePath, obj) {
  fs.writeFileSync(filePath, JSON.stringify(obj, null, 2));
}

const resultPath = '/agentlab/out/grader_result.json';
const reportPath = '/agentlab/out/demo_grader_report.json';

if (!fs.existsSync(resultPath)) {
  throw new Error(`${resultPath} is required`);
}
const result = readJson(resultPath);
const value = Number(result.metrics?.difficulty_match ?? 0);

writeJson(reportPath, {
  resolved: value,
  agent_outcome: result.outcome || null,
  grader: 'agentlab_demo_grader',
});
