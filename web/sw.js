// Minimal service worker: makes the app installable. The portal is a live control panel,
// so nothing is cached; every request goes to the network.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
self.addEventListener('fetch', (e) => {
  e.respondWith(
    fetch(e.request).catch(
      () => new Response('WebDock Dev is not running. Start it from the desktop shortcut or tray icon.', { status: 503, headers: { 'Content-Type': 'text/plain' } })
    )
  );
});
