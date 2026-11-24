// Drives a running server through the official meilisearch-js client.
import { Meilisearch } from 'meilisearch';
import assert from 'node:assert/strict';

const client = new Meilisearch({ host: 'http://127.0.0.1:7711', apiKey: 'test-master-key' });
const uid = 'sdk_books';
let pass = 0;
const failures = [];

async function check(name, fn) {
  try {
    await fn();
    pass++;
    console.log('ok    ' + name);
  } catch (e) {
    failures.push(name);
    console.log('FAIL  ' + name + ' -> ' + String(e && e.message ? e.message : e).split('\n')[0].slice(0, 220));
  }
}
const done = async (p) => {
  const t = await p.waitTask();
  assert.equal(t.status, 'succeeded', JSON.stringify(t.error));
  return t;
};

const books = [
  { id: 1, title: 'Cien años de soledad', author: 'Gabriel García Márquez', year: 1967, genres: ['novela', 'realismo mágico'], price: 12.5 },
  { id: 2, title: 'La casa de los espíritus', author: 'Isabel Allende', year: 1982, genres: ['novela'], price: 10 },
  { id: 3, title: 'Rayuela', author: 'Julio Cortázar', year: 1963, genres: ['novela', 'experimental'], price: 14 },
  { id: 4, title: 'Veinte poemas de amor y una canción desesperada', author: 'Pablo Neruda', year: 1924, genres: ['poesía'], price: 8 },
  { id: 5, title: 'Los detectives salvajes', author: 'Roberto Bolaño', year: 1998, genres: ['novela'], price: 16 },
];

await check('health / isHealthy', async () => { assert.equal((await client.health()).status, 'available'); assert.equal(await client.isHealthy(), true); });
await check('getVersion', async () => { assert.ok((await client.getVersion()).pkgVersion); });
await check('deleteIndexIfExists (missing index)', async () => { await client.deleteIndexIfExists(uid); });
await check('createIndex + waitTask', async () => { const t = await done(client.createIndex(uid, { primaryKey: 'id' })); assert.equal(t.type, 'indexCreation'); });
await check('getIndex / getRawInfo', async () => { const ix = await client.getIndex(uid); assert.equal(ix.primaryKey, 'id'); assert.equal((await client.index(uid).getRawInfo()).uid, uid); });
await check('getIndexes', async () => { const r = await client.getIndexes({ limit: 50 }); assert.ok(r.results.some((i) => i.uid === uid)); });
const index = client.index(uid);
await check('addDocuments', async () => { const t = await done(index.addDocuments(books)); assert.equal(t.details.indexedDocuments, 5); });
await check('addDocumentsInBatches', async () => { const ps = index.addDocumentsInBatches([{ id: 6, title: 'Temporal', year: 2000 }, { id: 7, title: 'Temporal dos', year: 2001 }], 1); for (const p of ps) await done(p); });
await check('updateSettings', async () => { await done(index.updateSettings({ filterableAttributes: ['genres', 'year', 'author'], sortableAttributes: ['year', 'price'], stopWords: ['de', 'la', 'los'] })); });
await check('getSettings', async () => { const s = await index.getSettings(); assert.deepEqual(s.sortableAttributes, ['year', 'price']); assert.ok(Array.isArray(s.rankingRules)); });
await check('get/update/reset single settings', async () => {
  await done(index.updateSynonyms({ libro: ['novela'] }));
  assert.deepEqual(await index.getSynonyms(), { libro: ['novela'] });
  await done(index.updateTypoTolerance({ minWordSizeForTypos: { oneTypo: 4 } }));
  assert.equal((await index.getTypoTolerance()).minWordSizeForTypos.oneTypo, 4);
  await done(index.resetTypoTolerance());
  assert.equal((await index.getTypoTolerance()).minWordSizeForTypos.oneTypo, 5);
  assert.deepEqual((await index.getFilterableAttributes()).sort(), ['author', 'genres', 'year']);
  assert.deepEqual(await index.getSearchableAttributes(), ['*']);
  assert.deepEqual(await index.getDisplayedAttributes(), ['*']);
  assert.equal((await index.getPagination()).maxTotalHits, 1000);
  assert.equal((await index.getFaceting()).maxValuesPerFacet, 100);
  assert.equal(await index.getDistinctAttribute(), null);
  assert.deepEqual((await index.getStopWords()).sort(), ['de', 'la', 'los']);
  await done(index.updateRankingRules(['words', 'typo', 'proximity', 'attribute', 'sort', 'exactness']));
});
await check('search: typo + accents', async () => { const r = await index.search('garsia marques'); assert.equal(r.hits[0].id, 1); assert.equal(typeof r.processingTimeMs, 'number'); assert.equal(typeof r.estimatedTotalHits, 'number'); });
await check('search: filter + sort + facets', async () => {
  const r = await index.search('', { filter: ['genres = novela', 'year > 1965'], sort: ['year:desc'], facets: ['genres'] });
  assert.deepEqual(r.hits.map((h) => h.id), [5, 2, 1]);
  assert.equal(r.facetDistribution.genres.novela, 3);
});
await check('search: phrase', async () => { const r = await index.search('"poemas de amor"'); assert.deepEqual(r.hits.map((h) => h.id), [4]); assert.equal((await index.search('"amor poemas"')).hits.length, 0); });
await check('search: highlight, crop, retrieve, matches', async () => {
  const r = await index.search('soledad', { attributesToRetrieve: ['title'], attributesToHighlight: ['title'], highlightPreTag: '<b>', highlightPostTag: '</b>', showMatchesPosition: true });
  assert.deepEqual(Object.keys(r.hits[0]).sort(), ['_formatted', '_matchesPosition', 'title']);
  assert.equal(r.hits[0]._formatted.title, 'Cien años de <b>soledad</b>');
  assert.ok(r.hits[0]._matchesPosition.title.length === 1);
});
await check('search: page / hitsPerPage', async () => { const r = await index.search('', { page: 2, hitsPerPage: 2 }); assert.equal(r.hits.length, 2); assert.equal(r.totalHits, 7); assert.equal(r.totalPages, 4); assert.equal(r.page, 2); });
await check('searchGet', async () => { const r = await index.searchGet('rayuela', { filter: 'year < 1970', attributesToRetrieve: ['id'] }); assert.deepEqual(r.hits, [{ id: 3 }]); });
await check('search: synonym', async () => { const r = await index.search('libro', { limit: 20 }); assert.ok(r.hits.length >= 4); });
await check('search: bad filter gives a typed error', async () => {
  await assert.rejects(index.search('', { filter: 'title = x' }), (e) => { assert.equal(e.cause.code, 'invalid_search_filter'); return true; });
});
await check('multiSearch', async () => { const r = await client.multiSearch({ queries: [{ indexUid: uid, q: 'neruda' }, { indexUid: uid, q: 'bolaño' }] }); assert.equal(r.results.length, 2); assert.equal(r.results[0].indexUid, uid); assert.equal(r.results[1].hits[0].id, 5); });
await check('getDocument', async () => { assert.equal((await index.getDocument(3)).title, 'Rayuela'); assert.deepEqual(await index.getDocument(3, { fields: ['id'] }), { id: 3 }); });
await check('getDocuments (query + filter)', async () => {
  const r = await index.getDocuments({ limit: 3, offset: 1, fields: ['id'] });
  assert.equal(r.results.length, 3); assert.equal(r.total, 7);
  const f = await index.getDocuments({ filter: 'author = "Isabel Allende"' });
  assert.equal(f.results[0].id, 2);
});
await check('updateDocuments (partial)', async () => { await done(index.updateDocuments([{ id: 2, price: 11.25 }])); const d = await index.getDocument(2); assert.equal(d.price, 11.25); assert.equal(d.author, 'Isabel Allende'); });
await check('deleteDocument', async () => { await done(index.deleteDocument(6)); await assert.rejects(index.getDocument(6), (e) => { assert.equal(e.cause.code, 'document_not_found'); return true; }); });
await check('deleteDocuments (ids)', async () => { const t = await done(index.deleteDocuments([7])); assert.equal(t.details.deletedDocuments, 1); });
await check('deleteDocuments (filter)', async () => { const t = await done(index.deleteDocuments({ filter: 'year < 1930' })); assert.equal(t.details.deletedDocuments, 1); });
await check('getStats (index and global)', async () => { assert.equal((await index.getStats()).numberOfDocuments, 4); const g = await client.getStats(); assert.equal(g.indexes[uid].numberOfDocuments, 4); });
await check('tasks.getTasks / getTask', async () => {
  const r = await client.tasks.getTasks({ indexUids: [uid], limit: 3 });
  assert.equal(r.results.length, 3); assert.ok(r.results[0].uid > r.results[1].uid);
  const t = await client.tasks.getTask(r.results[0].uid); assert.equal(t.uid, r.results[0].uid);
  // Deleting the missing index at the start is recorded as a failed task, as in Meilisearch.
  const failed = await client.tasks.getTasks({ statuses: ['failed'], indexUids: [uid] });
  assert.equal(failed.total, failed.results.length);
  assert.ok(failed.results.length >= 1 && failed.results.every((t) => t.status === 'failed' && t.error.code === 'index_not_found'));
});
await check('failed task carries an error', async () => { const t = await client.createIndex(uid).waitTask(); assert.equal(t.status, 'failed'); assert.equal(t.error.code, 'index_already_exists'); });
await check('keys: create, get, update, restricted use, delete', async () => {
  const k = await client.createKey({ name: 'solo libros', actions: ['search'], indexes: [uid], expiresAt: null });
  assert.ok(k.key && k.uid);
  assert.equal((await client.getKey(k.uid)).name, 'solo libros');
  assert.equal((await client.updateKey(k.uid, { description: 'x' })).description, 'x');
  assert.ok((await client.getKeys({ limit: 50 })).results.some((x) => x.uid === k.uid));
  const limited = new Meilisearch({ host: 'http://127.0.0.1:7711', apiKey: k.key });
  assert.equal((await limited.index(uid).search('rayuela')).hits[0].id, 3);
  await assert.rejects(limited.index('products').search('x'), (e) => { assert.equal(e.cause.code, 'invalid_api_key'); return true; });
  await assert.rejects(limited.index(uid).addDocuments([{ id: 99 }]), (e) => { assert.equal(e.cause.code, 'invalid_api_key'); return true; });
  await client.deleteKey(k.uid);
  await assert.rejects(limited.index(uid).search('rayuela'));
});
await check('updateIndex / fetchPrimaryKey', async () => { assert.equal(await index.fetchPrimaryKey(), 'id'); });
await check('deleteAllDocuments', async () => { await done(index.deleteAllDocuments()); assert.equal((await index.getStats()).numberOfDocuments, 0); });
await check('resetSettings', async () => { await done(index.resetSettings()); assert.deepEqual(await index.getFilterableAttributes(), []); });
await check('deleteIndex', async () => { await done(client.deleteIndex(uid)); await assert.rejects(client.getIndex(uid), (e) => { assert.equal(e.cause.code, 'index_not_found'); return true; }); });

console.log(`\n${pass} passed, ${failures.length} failed`);
if (failures.length) console.log('failed: ' + failures.join(' | '));
