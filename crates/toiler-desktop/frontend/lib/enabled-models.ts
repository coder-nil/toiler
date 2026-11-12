/**
 * `enabledModels` 白名单生成。
 *
 * 对话框的模型选择列表由 pi 的 `enabledModels` 设置决定（与 `--models` 同一套
 * 语法，见 lib/model-scope.ts）。设置页允许用户按服务商勾选模型，这里负责把
 * 勾选结果翻译回尽量简短的 pattern 列表：
 *
 *   - 整个服务商全部勾选 → `provider/*`（顺带保留 `:level` 固定）
 *   - 只勾选一部分       → 逐个 `provider/modelId`
 *   - 一个都没勾选       → 该服务商不写条目，模型全部隐藏
 *   - 所有服务商全选     → `undefined`，即恢复“未设置白名单”的默认状态
 *
 * 注意 `[]` 与 `undefined` 语义完全不同：pi 在 pattern 解析不出任何模型时会
 * 回退成“全部模型可见”，所以调用方绝不能把 `[]` 写进设置（见
 * app/api/models-config/enabled/route.ts 的校验）。
 */

export interface EnabledModelRef {
  provider: string;
  id: string;
  /** `enabledModels` 已为该模型固定的 thinking level（例如 `anthropic/*:high`）。 */
  thinkingLevel?: string;
}

export function modelKey(model: { provider: string; id: string }): string {
  return `${model.provider}/${model.id}`;
}

const THINKING_LEVEL_SUFFIXES = new Set([
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
]);

function qualifiedPattern(pattern: string): { provider: string; model: string } | undefined {
  let base = pattern.trim();
  const colonIndex = base.lastIndexOf(":");
  if (colonIndex > 0 && THINKING_LEVEL_SUFFIXES.has(base.slice(colonIndex + 1))) {
    base = base.slice(0, colonIndex);
  }

  const slashIndex = base.indexOf("/");
  if (slashIndex <= 0 || slashIndex === base.length - 1) return undefined;
  return {
    provider: base.slice(0, slashIndex),
    model: base.slice(slashIndex + 1),
  };
}

/** Remove every provider-qualified pattern, including `provider/*:level`. */
export function removeProviderFromEnabledModels(
  patterns: readonly string[],
  provider: string,
): string[] {
  const target = provider.toLowerCase();
  return patterns.filter((pattern) => {
    const parsed = qualifiedPattern(pattern);
    return !parsed || parsed.provider.toLowerCase() !== target;
  });
}

/** Remove exact model patterns while preserving a provider wildcard. */
export function removeModelFromEnabledModels(
  patterns: readonly string[],
  provider: string,
  modelId: string,
): string[] {
  const targetProvider = provider.toLowerCase();
  const targetModel = modelId.toLowerCase();
  return patterns.filter((pattern) => {
    const parsed = qualifiedPattern(pattern);
    if (!parsed || parsed.provider.toLowerCase() !== targetProvider) return true;
    return parsed.model === "*" || parsed.model.toLowerCase() !== targetModel;
  });
}

/** provider 名含 glob 元字符时不能简写成 `provider/*`。 */
export function isGlobSafeProviderName(provider: string): boolean {
  return !/[*?[\]{}()!+|@,]/.test(provider);
}

function patternFor(ref: EnabledModelRef): string {
  const level = ref.thinkingLevel ? `:${ref.thinkingLevel}` : "";
  return `${ref.provider}/${ref.id}${level}`;
}

/** 该服务商所有模型固定同一个 thinking level 时才能写成 `provider/*[:level]`。 */
function providerWildcard(models: readonly EnabledModelRef[]): string | undefined {
  const provider = models[0].provider;
  const levels = new Set(models.map((model) => model.thinkingLevel ?? ""));
  if (levels.size !== 1) return undefined;
  const level = [...levels][0];
  return level ? `${provider}/*:${level}` : `${provider}/*`;
}

/**
 * 把“哪些模型应该出现在模型选择列表里”编译成 `enabledModels` pattern。
 *
 * `available` 必须是全部可用模型（不只目标服务商），否则无法判断“某个服务商
 * 是否全选”。
 */
export function buildEnabledModelPatterns(
  available: readonly EnabledModelRef[],
  selected: readonly string[],
): string[] | undefined {
  if (available.length === 0) return undefined;

  const selectedKeys = new Set(selected);
  const byProvider = new Map<string, EnabledModelRef[]>();
  for (const model of available) {
    const bucket = byProvider.get(model.provider);
    if (bucket) bucket.push(model);
    else byProvider.set(model.provider, [model]);
  }

  const patterns: string[] = [];
  let everyModelSelected = true;

  for (const models of byProvider.values()) {
    const picked = models.filter((model) => selectedKeys.has(modelKey(model)));
    if (picked.length !== models.length) everyModelSelected = false;
    if (picked.length === 0) continue;

    if (picked.length === models.length && isGlobSafeProviderName(models[0].provider)) {
      const wildcard = providerWildcard(models);
      if (wildcard) {
        patterns.push(wildcard);
        continue;
      }
    }
    for (const model of picked) patterns.push(patternFor(model));
  }

  // 全选时回到“未设置白名单”：除非存在 thinking level 固定，那种情况下
  // undefined 会静静地把用户的固定一并抹掉。
  if (everyModelSelected && !available.some((model) => model.thinkingLevel)) return undefined;
  return patterns;
}

/**
 * 用某个服务商的勾选结果替换现有可见集合里该服务商的部分。
 *
 * 其他服务商的可见性保持不变：设置页一次只编辑一个服务商，不应该意外改变
 * 其它服务商的模型列表。
 */
export function applyProviderSelection(input: {
  visible: readonly { provider: string; id: string }[];
  provider: string;
  modelIds: readonly string[];
}): string[] {
  const next = input.visible
    .filter((model) => model.provider !== input.provider)
    .map(modelKey);
  for (const id of new Set(input.modelIds)) next.push(`${input.provider}/${id}`);
  return next;
}
