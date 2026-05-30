// Ocean Surface service worker — minimal, installability + shell cache only.
//
// Strategy: network-first for everything, cache the static shell as a
// fallback. NEVER touch /v1/* or /api/* — those are live daemon/proxy calls
// (turns, SSE, STT, TTS, config) and must always go to the network.

const CACHE = 'ocean-shell-v1';
const SHELL = ['/', '/index.html', '/manifest.webmanifest', '/icon-192.png', '/icon-512.png'];

self.addEventListener('install', (event) => {
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).catch(() => {}));
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))
    )
  );
  self.clients.claim();
});

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

  // Network-first, fall back to cache (so a flaky connection still launches).
  event.respondWith(
    fetch(event.request)
      .then((resp) => {
        const copy = resp.clone();
        caches.open(CACHE).then((c) => c.put(event.request, copy)).catch(() => {});
        return resp;
      })
      .catch(() => caches.match(event.request).then((m) => m || caches.match('/')))
  );
});
