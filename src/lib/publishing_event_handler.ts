/**
 * Composable sinks for the publishing event stream.
 *
 * A `PublishingEventHandler` is anything that can receive a `PublishingEvent`. Handlers
 * compose: `multi_handler` fans out, `masking_handler` redacts secrets then forwards.
 * Emission is best-effort and synchronous — an observability sink must never fail or
 * slow a run. The default sink is `null_handler` (drops everything).
 *
 * @module
 */

import type {PublishingEvent} from './publishing_event.js';

/** A sink for publishing events. */
export interface PublishingEventHandler {
	emit: (event: PublishingEvent) => void;
}

/** A `capture_handler` also exposes the events it has collected. */
export interface CapturingEventHandler extends PublishingEventHandler {
	readonly events: Array<PublishingEvent>;
}

/** Drops every event. The default when no handler is supplied. */
export const null_handler = (): PublishingEventHandler => ({
	emit: () => {},
});

/** Collects events in memory. Used to build the run report and in tests. */
export const capture_handler = (): CapturingEventHandler => {
	const events: Array<PublishingEvent> = [];
	return {
		events,
		emit: (event) => {
			events.push(event);
		},
	};
};

/**
 * Writes each event as one JSON object per line (JSON-lines) to `process.stdout`.
 * Write failures are swallowed — the stream is observability, not control flow.
 */
export const stdout_handler = (): PublishingEventHandler => ({
	emit: (event) => {
		try {
			process.stdout.write(JSON.stringify(event) + '\n');
		} catch {
			// best-effort: a logging sink must never fail a run
		}
	},
});

/** Fans an event out to every handler in order. */
export const multi_handler = (
	handlers: Array<PublishingEventHandler>,
): PublishingEventHandler => ({
	emit: (event) => {
		for (const handler of handlers) {
			handler.emit(event);
		}
	},
});

/**
 * Wraps a handler, masking secrets in each event's string fields before forwarding.
 *
 * @param inner - the handler to forward masked events to
 * @param mask - the masking function, defaults to `mask_secrets`
 */
export const masking_handler = (
	inner: PublishingEventHandler,
	mask: (event: PublishingEvent) => PublishingEvent = mask_secrets,
): PublishingEventHandler => ({
	emit: (event) => {
		inner.emit(mask(event));
	},
});

// Minimal redaction rules: npm auth tokens (bare or registry-scoped), `SECRET_*`
// env-style assignments, and `npm_`-prefixed tokens. Deliberately lean — error
// strings can carry npm/git output; a fuller secret catalog is deferred.
const SECRET_RULES: Array<readonly [RegExp, string]> = [
	[/((?:\/\/[^\s:]+:)?_authToken\s*=\s*)\S+/gi, '$1[redacted]'],
	[/(SECRET_[A-Z0-9_]+\s*[=:]\s*)\S+/g, '$1[redacted]'],
	[/(npm_[A-Za-z0-9]{4})[A-Za-z0-9]{12,}/g, '$1[redacted]'],
];

/** Redacts known secret shapes from a string. */
export const redact_secrets = (text: string): string =>
	SECRET_RULES.reduce((acc, [pattern, replacement]) => acc.replace(pattern, replacement), text);

/** Returns a copy of the event with secrets redacted from its string-valued fields. */
export const mask_secrets = (event: PublishingEvent): PublishingEvent => {
	const masked: Record<string, unknown> = {};
	for (const [key, value] of Object.entries(event)) {
		masked[key] = typeof value === 'string' ? redact_secrets(value) : value;
	}
	return masked as PublishingEvent;
};
