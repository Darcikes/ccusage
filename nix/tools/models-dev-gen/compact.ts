/**
 * Selection rules shared by the models.dev snapshot generator and its tests.
 *
 * models.dev publishes one catalog per provider, so the same model appears
 * dozens of times: once from whoever authored it, once per cloud platform that
 * hosts it, and once per reseller that marks it up or discounts it. Only the
 * authoring catalog is guaranteed to carry list pricing, so picking the wrong
 * duplicate silently bills users at a marketplace rate.
 */

/**
 * Trust tiers used to break ties between duplicate pricing keys. Higher wins.
 */
export const MODELS_DEV_PROVIDER_TRUST = {
	/** The catalog of whoever authored the model. Publishes list pricing. */
	owner: 3,
	/**
	 * Cloud platforms that resell at list price plus a documented regional
	 * premium. They are the only source for the platform-specific model ids
	 * agents record, such as the Bedrock `us.anthropic.*` inference profiles.
	 */
	platform: 2,
	/**
	 * Everyone else. Their prices are their own - promotions, markups, and
	 * third-party GPU hosts undercutting the author - so they are a last resort
	 * for models no trusted catalog publishes any more, such as retired Claude 3
	 * releases that only resellers still list.
	 */
	reseller: 1,
} as const satisfies Record<string, number>;

/**
 * Provider ids that host models at list price plus a published regional
 * premium, rather than setting their own.
 */
const PLATFORM_PROVIDER_IDS = [
	'amazon-bedrock',
	'azure',
	'azure-cognitive-services',
	'google-vertex',
	'google-vertex-anthropic',
] as const satisfies readonly string[];

/**
 * Provider ids that serve their own models under a different name than the
 * `models/<author>/` directory they are authored in, so the directory scan
 * alone cannot recognize them as first-party.
 */
const FIRST_PARTY_PROVIDER_ID_ALIASES = [
	// Z.ai authors GLM under `models/zhipuai/` but serves it as `zai`.
	'zai',
] as const satisfies readonly string[];

export type ModelsDevModalities = {
	input?: readonly string[];
	output?: readonly string[];
};

/**
 * Index of the canonical `models/<author>/<id>.toml` catalog, used to decide
 * which provider authored a model without hardcoding a provider list.
 */
export type ModelsDevCatalogIndex = {
	/** `<author>/<id>` keys, exactly as `generateModels` returns them. */
	authoredKeys: ReadonlySet<string>;
	/** Directory names under `models/`, i.e. the set of authoring providers. */
	authorProviderIds: ReadonlySet<string>;
	/** The same keys with the author prefix stripped. */
	authoredModelIds: ReadonlySet<string>;
	/** `authoredModelIds` normalized for prefix comparison. */
	normalizedAuthoredModelIds: readonly string[];
	/** Normalized authored id -> the modes that id publishes its own rates for. */
	authoredModes: ReadonlyMap<string, ReadonlySet<string>>;
	/** Authored modalities by bare model id. */
	authoredModalities: ReadonlyMap<string, ModelsDevModalities>;
};

export type ModelsDevPricingCandidate = {
	sourceProviderId: string;
	sourceModelId: string;
	trust: number;
	hasContextLimit: boolean;
	hasExplicitCacheRead: boolean;
	hasExplicitCacheWrite: boolean;
};

/**
 * Build the authorship index from the canonical catalog.
 *
 * `providerCatalogs` is read only for the modes an authoring provider prices
 * itself, because those live in `providers/<author>/models/<id>.toml` rather
 * than in the `models/` metadata.
 *
 * @param authoredModels - `<author>/<id>` keyed models from `generateCatalog().models`.
 * @param providerCatalogs - provider id keyed catalogs from `generateCatalog().providers`.
 * @example
 * const index = buildModelsDevCatalogIndex({ 'anthropic/claude-opus-5': {} });
 * index.authorProviderIds.has('anthropic'); // true
 */
export function buildModelsDevCatalogIndex(
	authoredModels: Readonly<Record<string, { modalities?: ModelsDevModalities }>>,
	providerCatalogs: Readonly<
		Record<
			string,
			{
				models?: Readonly<
					Record<string, { experimental?: { modes?: Readonly<Record<string, unknown>> } }>
				>;
			}
		>
	> = {},
): ModelsDevCatalogIndex {
	const authorProviderIds = new Set<string>();
	const authoredModelIds = new Set<string>();
	const authoredModalities = new Map<string, ModelsDevModalities>();
	for (const [key, model] of Object.entries(authoredModels)) {
		const separator = key.indexOf('/');
		if (separator <= 0) {
			continue;
		}
		const modelId = key.slice(separator + 1);
		authorProviderIds.add(key.slice(0, separator));
		authoredModelIds.add(modelId);
		if (model.modalities != null) {
			authoredModalities.set(modelId, model.modalities);
		}
	}
	const authoredModes = new Map<string, Set<string>>();
	for (const [providerId, catalog] of Object.entries(providerCatalogs)) {
		if (
			!authorProviderIds.has(providerId) &&
			!(FIRST_PARTY_PROVIDER_ID_ALIASES as readonly string[]).includes(providerId)
		) {
			continue;
		}
		for (const [modelId, model] of Object.entries(catalog.models ?? {})) {
			const modes = Object.keys(model.experimental?.modes ?? {}).map(normalizeModelId);
			if (modes.length === 0) {
				continue;
			}
			const normalized = normalizeModelId(modelId);
			const existing = authoredModes.get(normalized) ?? new Set<string>();
			for (const mode of modes) {
				existing.add(mode);
			}
			authoredModes.set(normalized, existing);
		}
	}
	return {
		authoredKeys: new Set(Object.keys(authoredModels)),
		authorProviderIds,
		authoredModelIds,
		normalizedAuthoredModelIds: [...authoredModelIds].map(normalizeModelId),
		authoredModes,
		authoredModalities,
	};
}

/** Model ids are spelled with either dots or dashes for the same version. */
function normalizeModelId(modelId: string): string {
	return modelId.toLowerCase().replace(/\./g, '-');
}

/**
 * Whether a reseller-only id names a tier of a model the snapshot already
 * carries - `kimi-k2.6-nitro`, `glm-5.2-flex`, `claude-opus-4-6-think`.
 *
 * Those are separately priced routes, usually cheaper than the author's list
 * rate, so resolving them to the base model by name similarity over-reports
 * their cost. Only bare ids qualify: an id carrying a provider path is that
 * provider's own catalogue entry for a model, not a distinct tier of it.
 *
 * A tier the author prices itself does not qualify, because then the reseller's
 * rate is a markup on a published rate rather than the only rate available:
 * `claude-opus-5-fast` exists solely in one reseller catalog at 12 USD/Mtok
 * while Anthropic's own fast rate is 10.
 */
export function isTierVariantOfAuthoredModel(
	sourceModelId: string,
	index: ModelsDevCatalogIndex,
): boolean {
	if (sourceModelId.includes('/')) {
		return false;
	}
	const normalized = normalizeModelId(sourceModelId);
	return index.normalizedAuthoredModelIds.some((authored) => {
		if (!normalized.startsWith(`${authored}-`)) {
			return false;
		}
		const tier = normalized.slice(authored.length + 1);
		return !index.authoredModes.get(authored)?.has(tier);
	});
}

/**
 * Trust tier for one provider catalog entry.
 *
 * @example
 * modelsDevProviderTrust({ providerId: 'openrouter', sourceModelId: 'kimi-k3', index });
 * // MODELS_DEV_PROVIDER_TRUST.reseller
 */
export function modelsDevProviderTrust({
	providerId,
	sourceModelId,
	index,
}: {
	providerId: string;
	sourceModelId: string;
	index: ModelsDevCatalogIndex;
}): number {
	// An exact `<provider>/<model>` hit in the authored catalog is the strongest
	// signal. The namespace check covers models a provider serves without a
	// canonical metadata file of their own, such as `openai/gpt-5.6`.
	if (
		index.authoredKeys.has(`${providerId}/${sourceModelId}`) ||
		index.authorProviderIds.has(providerId) ||
		(FIRST_PARTY_PROVIDER_ID_ALIASES as readonly string[]).includes(providerId)
	) {
		return MODELS_DEV_PROVIDER_TRUST.owner;
	}
	if ((PLATFORM_PROVIDER_IDS as readonly string[]).includes(providerId)) {
		return MODELS_DEV_PROVIDER_TRUST.platform;
	}
	return MODELS_DEV_PROVIDER_TRUST.reseller;
}

/**
 * Whether a candidate belongs in the embedded snapshot at all.
 *
 * Trusted catalogs are always embedded. A reseller catalog is embedded when the
 * authored catalog knows the model - which is what keeps retired first-party
 * releases only resellers still list - or when the id names a separately priced
 * tier of a model already carried. Everything else is a reseller's own alias for
 * a model some trusted catalog publishes anyway.
 *
 * @example
 * isEmbeddableModelsDevCandidate({ trust: 1, sourceModelId: 'accounts/fireworks/models/kimi-k2p6', index });
 * // false
 */
export function isEmbeddableModelsDevCandidate({
	trust,
	sourceModelId,
	index,
}: {
	trust: number;
	sourceModelId: string;
	index: ModelsDevCatalogIndex;
}): boolean {
	if (trust > MODELS_DEV_PROVIDER_TRUST.reseller) {
		return true;
	}
	return (
		index.authoredModelIds.has(sourceModelId) || isTierVariantOfAuthoredModel(sourceModelId, index)
	);
}

/**
 * The selection rules the Rust runtime loader needs, for the same decisions this
 * module makes at generation time. The runtime sees only the live `api.json`, so
 * it can neither scan the authored catalog for authorship nor read authored
 * modalities, and both have to be carried in.
 */
export function modelsDevCatalogRulesArtifact(index: ModelsDevCatalogIndex): {
	owners: string[];
	platforms: string[];
	assetPricedModelIds: string[];
} {
	const assetPricedModelIds = [...index.authoredModelIds]
		.filter((sourceModelId) => !isTokenPricedModel({ sourceModelId, modalities: undefined, index }))
		.sort();
	return {
		owners: [...index.authorProviderIds, ...FIRST_PARTY_PROVIDER_ID_ALIASES].sort(),
		platforms: [...PLATFORM_PROVIDER_IDS].sort(),
		assetPricedModelIds,
	};
}

/**
 * Whether a models.dev cost block can price tokens at all.
 *
 * Flat-fee subscription catalogs such as `kimi-for-coding` publish all-zero
 * costs, which would otherwise embed as a free model.
 */
export function isPriceableModelsDevCost<
	Value extends { input?: number | null; output?: number | null },
>(cost: Value): cost is Value & { input: number; output: number } {
	const { input, output } = cost;
	if (input == null || output == null) {
		return false;
	}
	return input !== 0 || output !== 0;
}

/**
 * Whether a model bills per text token, so the embedded `input` and `output`
 * rates mean what the runtime assumes when it multiplies them by token counts.
 *
 * Two signals, both read off the modalities:
 *
 * - Output must be text only. An image or audio output rate is per asset, so
 *   `gemini-2.5-flash-image`'s 30 USD output rate is per image, not per Mtok.
 * - Input must accept text. A model that accepts no text is a transcription or
 *   vision-only endpoint billed by duration - `whisper-large-v3` accepts audio
 *   alone and prices per second. Accepting audio, video, image or PDF *as well
 *   as* text is normal for chat models and is tokenised, so it stays eligible.
 *
 * The authored catalog decides, not the catalog being read. Reseller catalogs
 * describe the same model less carefully, and one claiming text-only output for
 * an image model would otherwise smuggle a per-image rate into the snapshot.
 *
 * @example
 * // authored as input: ["audio"], so excluded whichever catalog serves it
 * isTokenPricedModel({ sourceModelId: 'whisper-large-v3', modalities: { output: ['text'] }, index });
 * // false
 */
export function isTokenPricedModel({
	sourceModelId,
	modalities,
	index,
}: {
	sourceModelId: string;
	modalities: ModelsDevModalities | undefined;
	index: ModelsDevCatalogIndex;
}): boolean {
	const authoritative = index.authoredModalities.get(sourceModelId) ?? modalities;
	const output = authoritative?.output ?? ['text'];
	if (output.length !== 1 || output[0] !== 'text') {
		return false;
	}
	const input = authoritative?.input ?? ['text'];
	return input.includes('text');
}

export function selectModelsDevPricingKey(modelId: string, catalogId: string | undefined): string {
	return catalogId != null && catalogId.length > 0 ? catalogId : modelId;
}

export function shouldReplaceModelsDevPricingCandidate(
	existing: ModelsDevPricingCandidate,
	candidate: ModelsDevPricingCandidate,
): boolean {
	return compareModelsDevPricingCandidates(candidate, existing) > 0;
}

export function formatDuplicateModelsDevPricingKeyWarning({
	pricingKey,
	sourceModelId,
}: {
	pricingKey: string;
	sourceModelId: string;
}): string {
	return `models.dev pricing key "${pricingKey}" already exists; skipping duplicate source model "${sourceModelId}".`;
}

function compareModelsDevPricingCandidates(
	left: ModelsDevPricingCandidate,
	right: ModelsDevPricingCandidate,
): number {
	return (
		compareNumber(left.trust, right.trust) ||
		compareBoolean(left.hasExplicitCacheRead, right.hasExplicitCacheRead) ||
		compareBoolean(left.hasExplicitCacheWrite, right.hasExplicitCacheWrite) ||
		compareBoolean(left.hasContextLimit, right.hasContextLimit) ||
		compareStringPreferSmaller(left.sourceProviderId, right.sourceProviderId) ||
		compareStringPreferSmaller(left.sourceModelId, right.sourceModelId)
	);
}

function compareNumber(left: number, right: number): number {
	return left === right ? 0 : left > right ? 1 : -1;
}

function compareBoolean(left: boolean, right: boolean): number {
	return compareNumber(left ? 1 : 0, right ? 1 : 0);
}

function compareStringPreferSmaller(left: string, right: string): number {
	return left === right ? 0 : left < right ? 1 : -1;
}
