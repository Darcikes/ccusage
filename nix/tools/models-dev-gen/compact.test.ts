import assert from 'node:assert/strict';
import { it } from 'node:test';
import {
	buildModelsDevCatalogIndex,
	formatDuplicateModelsDevPricingKeyWarning,
	isEmbeddableModelsDevCandidate,
	isPriceableModelsDevCost,
	isTokenPricedModel,
	MODELS_DEV_PROVIDER_TRUST,
	modelsDevProviderTrust,
	modelsDevProviderTrustArtifact,
	shouldReplaceModelsDevPricingCandidate,
	selectModelsDevPricingKey,
} from './compact.ts';

const index = buildModelsDevCatalogIndex({
	'anthropic/claude-opus-5': { modalities: { input: ['text', 'image', 'pdf'], output: ['text'] } },
	'anthropic/claude-3-5-haiku-20241022': { modalities: { input: ['text'], output: ['text'] } },
	'moonshotai/kimi-k2.7-code': {
		modalities: { input: ['text', 'image', 'video'], output: ['text'] },
	},
	'xai/grok-build-0.1': { modalities: { input: ['text', 'image'], output: ['text'] } },
	'zhipuai/glm-5-turbo': { modalities: { input: ['text'], output: ['text'] } },
	'openai/whisper-large-v3': { modalities: { input: ['audio'], output: ['text'] } },
	'google/gemini-2.5-flash-image': {
		modalities: { input: ['text', 'image'], output: ['text', 'image'] },
	},
});

void it('indexes authoring providers and bare model ids from the catalog keys', () => {
	assert.deepEqual([...index.authorProviderIds].sort(), [
		'anthropic',
		'google',
		'moonshotai',
		'openai',
		'xai',
		'zhipuai',
	]);
	assert.equal(index.authoredModelIds.has('grok-build-0.1'), true);
	assert.equal(index.authoredModelIds.has('anthropic/claude-opus-5'), false);
});

void it('trusts the catalog that authored the model', () => {
	assert.equal(
		modelsDevProviderTrust({
			providerId: 'moonshotai',
			sourceModelId: 'kimi-k2.7-code',
			index,
		}),
		MODELS_DEV_PROVIDER_TRUST.owner,
	);
});

void it('trusts an authoring provider for models it serves without a catalog entry', () => {
	assert.equal(
		modelsDevProviderTrust({ providerId: 'anthropic', sourceModelId: 'claude-opus-9', index }),
		MODELS_DEV_PROVIDER_TRUST.owner,
	);
});

void it('trusts a first-party provider that renames its authoring namespace', () => {
	assert.equal(
		modelsDevProviderTrust({ providerId: 'zai', sourceModelId: 'glm-5-turbo', index }),
		MODELS_DEV_PROVIDER_TRUST.owner,
	);
});

void it('ranks cloud platforms below the author but above resellers', () => {
	assert.equal(
		modelsDevProviderTrust({
			providerId: 'amazon-bedrock',
			sourceModelId: 'us.anthropic.claude-opus-5',
			index,
		}),
		MODELS_DEV_PROVIDER_TRUST.platform,
	);
});

void it('ranks unknown catalogs as resellers', () => {
	assert.equal(
		modelsDevProviderTrust({ providerId: 'openrouter', sourceModelId: 'kimi-k2.7-code', index }),
		MODELS_DEV_PROVIDER_TRUST.reseller,
	);
});

void it('never lets a richer reseller entry replace the authoring catalog', () => {
	// Regression fence: models.dev lists kimi-k2.7-code at the MoonshotAI list
	// price and at a discounted OpenRouter price, and the reseller entry used to
	// win whenever it carried more fields.
	assert.equal(
		shouldReplaceModelsDevPricingCandidate(
			{
				sourceProviderId: 'moonshotai',
				sourceModelId: 'kimi-k2.7-code',
				trust: MODELS_DEV_PROVIDER_TRUST.owner,
				hasContextLimit: false,
				hasExplicitCacheRead: false,
				hasExplicitCacheWrite: false,
			},
			{
				sourceProviderId: 'openrouter',
				sourceModelId: 'kimi-k2.7-code',
				trust: MODELS_DEV_PROVIDER_TRUST.reseller,
				hasContextLimit: true,
				hasExplicitCacheRead: true,
				hasExplicitCacheWrite: true,
			},
		),
		false,
	);
});

void it('replaces a reseller entry with the authoring catalog', () => {
	assert.equal(
		shouldReplaceModelsDevPricingCandidate(
			{
				sourceProviderId: 'venice',
				sourceModelId: 'kimi-k2.7-code',
				trust: MODELS_DEV_PROVIDER_TRUST.reseller,
				hasContextLimit: true,
				hasExplicitCacheRead: true,
				hasExplicitCacheWrite: true,
			},
			{
				sourceProviderId: 'moonshotai',
				sourceModelId: 'kimi-k2.7-code',
				trust: MODELS_DEV_PROVIDER_TRUST.owner,
				hasContextLimit: true,
				hasExplicitCacheRead: true,
				hasExplicitCacheWrite: false,
			},
		),
		true,
	);
});

void it('uses a stable source ordering tie-break within one trust tier', () => {
	assert.equal(
		shouldReplaceModelsDevPricingCandidate(
			{
				sourceProviderId: 'nano-gpt',
				sourceModelId: 'claude-sonnet-4',
				trust: MODELS_DEV_PROVIDER_TRUST.reseller,
				hasContextLimit: true,
				hasExplicitCacheRead: true,
				hasExplicitCacheWrite: true,
			},
			{
				sourceProviderId: 'github-copilot',
				sourceModelId: 'claude-sonnet-4',
				trust: MODELS_DEV_PROVIDER_TRUST.reseller,
				hasContextLimit: true,
				hasExplicitCacheRead: true,
				hasExplicitCacheWrite: true,
			},
		),
		true,
	);
});

void it('keeps reseller entries only for models the catalog knows', () => {
	assert.equal(
		isEmbeddableModelsDevCandidate({
			trust: MODELS_DEV_PROVIDER_TRUST.reseller,
			sourceModelId: 'claude-3-5-haiku-20241022',
			index,
		}),
		true,
	);
	assert.equal(
		isEmbeddableModelsDevCandidate({
			trust: MODELS_DEV_PROVIDER_TRUST.reseller,
			sourceModelId: 'kimi-k2.6-nitro',
			index,
		}),
		false,
	);
});

void it('keeps platform entries whose ids only exist on that platform', () => {
	assert.equal(
		isEmbeddableModelsDevCandidate({
			trust: MODELS_DEV_PROVIDER_TRUST.platform,
			sourceModelId: 'us.anthropic.claude-sonnet-4-5-20250929-v1:0',
			index,
		}),
		true,
	);
});

void it('rejects flat-fee catalogs that publish all-zero token costs', () => {
	assert.equal(isPriceableModelsDevCost({ input: 0, output: 0 }), false);
	assert.equal(isPriceableModelsDevCost({ input: 0, output: 2 }), true);
	assert.equal(isPriceableModelsDevCost({ input: 1 }), false);
	assert.equal(isPriceableModelsDevCost({ input: 1, output: 2 }), true);
});

void it('accepts chat models whose audio and video inputs are tokenised', () => {
	// kimi-k2.7-code takes video input and gemini takes audio, both billed per
	// token, so a non-text input modality cannot disqualify a model on its own.
	assert.equal(
		isTokenPricedModel({ sourceModelId: 'claude-opus-5', modalities: undefined, index }),
		true,
	);
	assert.equal(
		isTokenPricedModel({ sourceModelId: 'kimi-k2.7-code', modalities: undefined, index }),
		true,
	);
	assert.equal(
		isTokenPricedModel({ sourceModelId: 'unlisted-model', modalities: undefined, index }),
		true,
	);
});

void it('rejects duration-priced models however the serving catalog describes them', () => {
	// whisper-large-v3 accepts no text at all and prices per second, but the
	// catalogs serving it advertise text output, so its rate would read as a token
	// rate.
	assert.equal(
		isTokenPricedModel({
			sourceModelId: 'whisper-large-v3',
			modalities: { input: ['audio'], output: ['text'] },
			index,
		}),
		false,
	);
});

void it('rejects image-output models a reseller catalog describes as text-only', () => {
	// google authors gemini-2.5-flash-image with image output; 302ai lists the same
	// model as text-only, which would embed a per-image rate as an output rate.
	assert.equal(
		isTokenPricedModel({
			sourceModelId: 'gemini-2.5-flash-image',
			modalities: { input: ['text', 'image'], output: ['text'] },
			index,
		}),
		false,
	);
});

void it('falls back to the serving catalog for models the authored catalog omits', () => {
	assert.equal(
		isTokenPricedModel({
			sourceModelId: 'us.anthropic.claude-opus-5',
			modalities: { input: ['text', 'image'], output: ['text'] },
			index,
		}),
		true,
	);
	assert.equal(
		isTokenPricedModel({
			sourceModelId: 'some-tts-model',
			modalities: { input: ['text'], output: ['audio'] },
			index,
		}),
		false,
	);
});

void it('exports sorted provider trust lists for the runtime loader', () => {
	assert.deepEqual(modelsDevProviderTrustArtifact(index), {
		owners: ['anthropic', 'google', 'moonshotai', 'openai', 'xai', 'zai', 'zhipuai'],
		platforms: [
			'amazon-bedrock',
			'azure',
			'azure-cognitive-services',
			'google-vertex',
			'google-vertex-anthropic',
		],
	});
});

void it('falls back to the source model id when the catalog id is empty', () => {
	assert.equal(
		selectModelsDevPricingKey('anthropic/claude-sonnet-4', ''),
		'anthropic/claude-sonnet-4',
	);
});

void it('falls back to the source model id when the catalog id is undefined', () => {
	assert.equal(
		selectModelsDevPricingKey('anthropic/claude-sonnet-4', undefined),
		'anthropic/claude-sonnet-4',
	);
});

void it('uses the catalog id when it is non-empty', () => {
	assert.equal(
		selectModelsDevPricingKey('anthropic/claude-sonnet-4', 'catalog-id-123'),
		'catalog-id-123',
	);
});

void it('formats duplicate pricing key warnings with the skipped source id', () => {
	assert.equal(
		formatDuplicateModelsDevPricingKeyWarning({
			pricingKey: 'claude-sonnet-4',
			sourceModelId: 'anthropic/claude-sonnet-4',
		}),
		'models.dev pricing key "claude-sonnet-4" already exists; skipping duplicate source model "anthropic/claude-sonnet-4".',
	);
});
