import type { EntityKind, JsonObject } from "./canonicalization";

export type NormalizationSeverity = "info" | "warning";

export type NormalizationAction =
  | "register_new"
  | "use_existing"
  | "create_alias"
  | "replace_alias"
  | "move_to_metadata"
  | "migrate_schema";

export interface NormalizationSuggestion {
  severity: NormalizationSeverity;
  code: string;
  message: string;
  pointer?: string;
  action?: NormalizationAction;
  relatedDigest?: string;
  patchPreview?: JsonObject[];
}

export interface NormalizationReport {
  applied: false;
  suggestions: NormalizationSuggestion[];
}

export interface NormalizationHintInput {
  kind: EntityKind;
  object: JsonObject;
  aliases?: string[];
  similarDigests?: Array<{
    digest: string;
    score: number;
    displayName?: string;
  }>;
}

export function normalizationHints(input: NormalizationHintInput): NormalizationReport {
  const suggestions: NormalizationSuggestion[] = [];

  if ("id" in input.object) {
    suggestions.push({
      severity: "info",
      code: "display_id_is_not_identity",
      message:
        "The id field is an authoring handle, not registry identity. The content digest remains the stable identity.",
      pointer: "/id",
      action: "create_alias",
    });
  }

  if ("name" in input.object) {
    suggestions.push({
      severity: "info",
      code: "name_is_presentation",
      message:
        "The name field may be useful for display, but changing it changes this object's digest unless you move it to external alias metadata.",
      pointer: "/name",
      action: "move_to_metadata",
    });
  }

  for (const match of input.similarDigests ?? []) {
    if (match.score <= 0) {
      continue;
    }
    suggestions.push({
      severity: "info",
      code: "similar_registered_entity",
      message: `This ${input.kind} is similar to ${match.displayName ?? match.digest}. Choose explicitly whether to use the existing entity or register a new one.`,
      action: "use_existing",
      relatedDigest: match.digest,
    });
  }

  for (const alias of input.aliases ?? []) {
    if (alias.trim().length === 0) {
      suggestions.push({
        severity: "warning",
        code: "empty_alias_ignored",
        message: "Empty aliases are not valid registry handles.",
        action: "create_alias",
      });
    }
  }

  return {
    applied: false,
    suggestions,
  };
}

