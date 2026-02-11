import {mkdir, readFile, rm, writeFile} from 'node:fs/promises';
import {dirname, join} from 'node:path';
import {paths} from '@fuzdev/gro/paths.js';
import {format_file} from '@fuzdev/gro/format_file.js';
import {deserialize_cache, serialize_cache, type FetchValueCache} from '@fuzdev/fuz_util/fetch.js';
import {existsSync} from 'node:fs';

// TODO upstream to Gro probably, and rename/redesign?

export interface FetchCache {
	name: string;
	data: FetchValueCache; // TODO probably expose an API for this instead of passing the map directly
	/**
	 * @returns true if anything changed, false if no-op
	 */
	save: () => Promise<boolean>;
}

/**
 * Creates file-system backed cache for fuz_util's fetch.js API responses.
 *
 * Cache invalidation strategy: If cache file can't be read or parsed, entire
 * cache is cleared (delete file) and starts fresh. This handles format changes.
 *
 * Uses `structuredClone` to track changes - only writes to disk if data modified.
 * Formatted with Prettier before writing for version control friendliness.
 *
 * @param name cache filename (without .json extension)
 * @param dir cache directory (defaults to `.gro/build/fetch/`)
 * @returns cache object with Map-based data and save() method
 */
export const create_fs_fetch_value_cache = async (
	name: string,
	dir = join(paths.build, 'fetch'),
): Promise<FetchCache> => {
	const data_path = join(dir, name + '.json');
	let data: FetchValueCache;
	if (existsSync(data_path)) {
		try {
			data = deserialize_cache(await readFile(data_path, 'utf8')); // TODO pass schema to parse so failures invalidate the cache
		} catch (_err) {
			// something went wrong, maybe the data format changed, so clear the cache
			data = new Map();
			await rm(data_path);
		}
	} else {
		data = new Map();
	}
	const initial = structuredClone(data);
	return {
		name,
		data,
		save: async () => {
			if (deep_equal_maps(initial, data)) {
				return false; // no changes to save
			}
			await mkdir(dirname(data_path), {recursive: true});
			await writeFile(data_path, await format_file(serialize_cache(data), {filepath: data_path}));
			return true;
		},
	};
};

// TODO this is quick and dirty, but fine because it's only expected to be called during development
const deep_equal_maps = (a: Map<unknown, unknown>, b: Map<unknown, unknown>): boolean => {
	if (a.size !== b.size) {
		return false;
	}
	for (const [key, value] of a) {
		if (!b.has(key) || JSON.stringify(b.get(key)) !== JSON.stringify(value)) {
			return false;
		}
	}
	return true;
};
