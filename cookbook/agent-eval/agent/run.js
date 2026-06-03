const fs = require("fs");
const path = require("path");

const profileIndex = process.argv.indexOf("--profile");
const profile = profileIndex >= 0 ? process.argv[profileIndex + 1] : "balanced";
const inputPath = process.env.BUCEPHALUS_TRIAL_INPUT_PATH;
const outputPath = process.env.BUCEPHALUS_RESULT_PATH || "/bucephalus/out/result.json";

if (!inputPath) {
  throw new Error("BUCEPHALUS_TRIAL_INPUT_PATH is required");
}

const trial = JSON.parse(fs.readFileSync(inputPath, "utf8"));
const inputs = trial.case?.inputs || trial.case?.input || {};
const prompt = String(inputs.prompt || "");
const expected = Array.isArray(inputs.expected_keywords) ? inputs.expected_keywords : [];
const hits = expected.filter((keyword) => prompt.toLowerCase().includes(String(keyword).toLowerCase()));
const resolved = expected.length === 0 ? 1 : hits.length / expected.length;

const result = {
  answer: {
    profile,
    summary: `Processed ${trial.ids?.case_id || "case"} with ${profile} profile.`,
  },
  metrics: {
    resolved,
    keyword_hits: hits.length,
  },
};

fs.mkdirSync(path.dirname(outputPath), { recursive: true });
fs.writeFileSync(outputPath, JSON.stringify(result, null, 2));

