// Release identity baked into cloud images at build time. Every deployed
// component reports this so version skew between API, worker, and pool
// controller is observable instead of silently producing contract mismatches.
export type ReleaseIdentity = {
  version: string | null;
  git_sha: string | null;
};

export function releaseIdentity(env: NodeJS.ProcessEnv = process.env): ReleaseIdentity {
  return {
    version: nonEmpty(env.BUCEPHALUS_RELEASE_VERSION),
    git_sha: nonEmpty(env.BUCEPHALUS_RELEASE_GIT_SHA),
  };
}

function nonEmpty(value: string | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : null;
}
