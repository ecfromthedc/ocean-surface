// Ocean Surface service worker — self-healing, fail-safe shell delivery.
//
// History (OCEAN — "blank pane / 11-minute load"): a wedged sw.js pinned a
// stale cached HTML shell, so the app showed a blank pane and "loaded" for
// minutes even though the server bytes were current and valid. The cause was
// twofold: (a) a slow-but-succeeding network fetch could be pre-empted by a
// stale cached copy, and (b) an OLD already-installed worker kept controlling
// the page and never handed off to a fresh deploy.
//
// This rewrite makes the worker SELF-HEALING and FAIL-SAFE:
//   • The HTML shell + sw.js are ALWAYS served network-fresh — a new deploy is
//     picked up immediately, never pinned behind an old worker.
//   • Navigations are network-FIRST with a short timeout: a slow tunnel fails
//     fast to a cached shell (seconds, not minutes) instead of hanging.
//   • A successful (even if slightly slow) network response is NEVER pre-empted
//     by stale cache — cache is only a last resort when the network is dead.
//   • Hashed static assets are immutable (content-addressed), so cache-first is
//     safe and fast for THOSE — they never change for a given hash.
//   • On activation the new worker claims all clients and tells them to reload,
//     so a deploy auto-updates the user with no manual cache-clear.
//
// NEVER touch /v1/* or /api/* — those are live daemon/proxy calls (turns, SSE,
// STT, TTS, config, permissions) and must always go straight to the network.

// Bump on every deploy to evict the prior shell. `activate` deletes every cache
// that isn't this one, so a version bump guarantees the wedged v3 cache (and
// any older shell) is dropped the instant this worker activates.
const CACHE = 'ocean-shell-v4';

// Precache the install-time PWA bits. The start_url ('/') is fetched on install
// and stored under a STABLE key (OFFLINE_KEY) — separate from live navigation
// responses — so Chrome's installable-PWA check sees a working offline
// start_url, while navigations stay network-first and never serve a stale shell.
const SHELL = ['/manifest.webmanifest', '/icon-192.png', '/icon-512.png'];
const OFFLINE_KEY = '/__offline_shell__';

// How long to wait for the network on a navigation before failing over to the
// cached shell. Short on purpose: a slow/flaky tunnel must fail fast (seconds)
// rather than hang the page for minutes. Once we time out we still serve a
// shell so the app boots; the in-page bundle then talks to the live daemon.
const NAV_TIMEOUT_MS = 5000;

// Fetch with a hard timeout. Resolves with the response if the network answers
// in time; rejects (AbortError) otherwise so the caller can fall back to cache.
function fetchWithTimeout(request, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  // Force a fresh fetch for the shell so a new deploy is always picked up; the
  // browser HTTP cache must never short-circuit the navigation document.
  return fetch(request, { signal: controller.signal, cache: 'no-store' })
    .finally(() => clearTimeout(timer));
}

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE).then(async (c) => {
      await c.addAll(SHELL).catch(() => {});
      // Seed the offline navigation fallback so the app is installable +
      // launchable with no network.
      try {
        const resp = await fetch('/', { cache: 'no-store' });
        if (resp.ok) await c.put(OFFLINE_KEY, resp.clone());
      } catch (_) {}
    })
  );
  // Take over immediately — don't wait for old tabs to close. Combined with the
  // activate-time claim + reload message below, a new deploy heals on the spot.
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    (async () => {
      // Drop every cache that isn't the current one — evicts the wedged v3
      // shell and any older bundle the instant this worker activates.
      const keys = await caches.keys();
      await Promise.all(
        keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))
      );
      await self.clients.claim();
      // Tell every controlled client a fresh worker is live so it can reload
      // onto the new shell without a manual cache-clear. Pairs with the
      // index.html controllerchange handler (belt-and-suspenders reload).
      const clientList = await self.clients.matchAll({ type: 'window' });
      for (const client of clientList) {
        client.postMessage({ type: 'SW_ACTIVATED', cache: CACHE });
      }
    })()
  );
});

// Hashed, content-addressed assets are immutable: the same URL always maps to
// the same bytes (Trunk emits e.g. `index-<hash>.js`,
// `ocean-surface-ui-<hash>_bg.wasm`, `<name>-<hash>.css`). For THOSE,
// cache-first is correct and fast. The HTML shell and sw.js carry no content
// hash and MUST stay network-fresh, so they are deliberately excluded here.
function isImmutableAsset(url) {
  // Hash is a hex run of 8+ chars right before the extension. wasm-bindgen
  // emits `<name>-<hash>_bg.wasm`, so allow an optional `_bg` between the hash
  // and `.wasm`. JS / CSS are `<name>-<hash>.{js,css}`.
  return /-[0-9a-f]{8,}(?:_bg)?\.(?:js|wasm|css)$/i.test(url.pathname);
}

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);

  // Bypass the SW entirely for live endpoints — let them hit the network
  // directly so SSE streaming and POSTs are never intercepted or cached.
  if (url.pathname.startsWith('/v1/') || url.pathname.startsWith('/api/')) {
    return;
  }

  // Only handle same-origin GETs; everything else passes through.
  if (event.request.method !== 'GET' || url.origin !== self.location.origin) {
    return;
  }

  // sw.js itself must ALWAYS be fetched fresh so a new worker can replace a
  // wedged one. Never serve it from cache (this is the escape hatch — an old
  // worker can't pin its own successor).
  if (url.pathname === '/sw.js') {
    event.respondWith(fetch(event.request, { cache: 'no-store' }));
    return;
  }

  // Navigations (the HTML doc): network-FIRST with a short timeout. A
  // successful response — even a slow one within the timeout — always wins and
  // refreshes the offline fallback. We fall back to the cached shell ONLY when
  // the network is dead or too slow (offline / timeout), so a wedged stale
  // shell can never pre-empt a live, current one. This is what stops the
  // "blank pane + 11-minute load": slow network fails fast to a shell that then
  // boots and talks to the live daemon, instead of hanging indefinitely.
  if (event.request.mode === 'navigate') {
    event.respondWith(
      fetchWithTimeout(event.request, NAV_TIMEOUT_MS)
        .then((resp) => {
          if (resp && resp.ok) {
            const copy = resp.clone();
            caches.open(CACHE).then((c) => c.put(OFFLINE_KEY, copy)).catch(() => {});
          }
          return resp;
        })
        .catch(async () =>
          // Offline / timed out: serve the freshest shell we have, else a
          // minimal inline boot page so the user never stares at a blank pane.
          (await caches.match(OFFLINE_KEY)) ||
          (await caches.match('/')) ||
          new Response(
            '<!doctype html><meta charset=utf-8>' +
              '<title>Ocean</title>' +
              '<body style="background:#06111d;color:#cfe;font:16px system-ui;' +
              'display:grid;place-items:center;height:100vh;margin:0">' +
              'Reconnecting…<script>setTimeout(()=>location.reload(),2500)</script>',
            { headers: { 'Content-Type': 'text/html; charset=utf-8' } }
          )
        )
    );
    return;
  }

  // Hashed, immutable assets: cache-first for speed, fall through to the
  // network (and populate the cache) on a miss. Safe because the URL is
  // content-addressed — a new build ships a NEW filename, so there is no stale
  // version to serve.
  if (isImmutableAsset(url)) {
    event.respondWith(
      caches.match(event.request).then(
        (hit) =>
          hit ||
          fetch(event.request).then((resp) => {
            if (resp && resp.ok) {
              const copy = resp.clone();
              caches.open(CACHE).then((c) => c.put(event.request, copy)).catch(() => {});
            }
            return resp;
          })
      )
    );
    return;
  }

  // Everything else (un-hashed assets, icons, manifest): network-first so a
  // deploy is reflected, with cache as an offline fallback.
  event.respondWith(
    fetch(event.request)
      .then((resp) => {
        if (resp && resp.ok) {
          const copy = resp.clone();
          caches.open(CACHE).then((c) => c.put(event.request, copy)).catch(() => {});
        }
        return resp;
      })
      .catch(() => caches.match(event.request))
  );
});
