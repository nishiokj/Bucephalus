const fs = require("fs");
const path = require("path");

const toneIndex = process.argv.indexOf("--tone");
const tone = toneIndex >= 0 ? process.argv[toneIndex + 1] : "balanced";
const inputPath = process.env.BUCEPHALUS_TRIAL_INPUT_PATH;
const outputPath = process.env.BUCEPHALUS_RESULT_PATH || "/bucephalus/out/result.json";

if (!inputPath) {
  throw new Error("BUCEPHALUS_TRIAL_INPUT_PATH is required");
}

const trial = JSON.parse(fs.readFileSync(inputPath, "utf8"));
const inputs = trial.case?.inputs || trial.case?.input || {};
const prompt = String(inputs.prompt || "");
const toneMultiplier = tone === "terse" ? 0.75 : tone === "expansive" ? 1.25 : 1;
const score = Math.min(1, Math.max(0, (prompt.length / 160) * toneMultiplier));

const result = {
  answer: {
    tone,
    summary: `${tone} response for ${trial.ids?.case_id || "case"}`,
  },
  metrics: {
    score,
    prompt_chars: prompt.length,
  },
};

fs.mkdirSync(path.dirname(outputPath), { recursive: true });
fs.writeFileSync(outputPath, JSON.stringify(result, null, 2));

