import { watch } from 'node:fs';

const root = new URL('../src/', import.meta.url);
const noCache = { 'Cache-Control': 'no-store' };
const server = Bun.serve({
  hostname: '127.0.0.1',
  port: 1420,
  async fetch(request, server) {
    const { pathname } = new URL(request.url);
    if (pathname === '/__reload') {
      if (server.upgrade(request)) return;
      return new Response('WebSocket required', { status: 400 });
    }
    if (pathname === '/__reload.js') {
      return new Response('new WebSocket("ws://" + location.host + "/__reload").onmessage = () => location.reload();', {
        headers: { ...noCache, 'Content-Type': 'text/javascript' },
      });
    }
    const name = pathname === '/' ? 'index.html' : pathname.slice(1);
    const url = new URL('./' + name, root);
    if (!url.href.startsWith(root.href)) return new Response(null, { status: 404 });
    const file = Bun.file(url);
    if (!(await file.exists())) return new Response(null, { status: 404 });
    if (name !== 'index.html') return new Response(file, { headers: noCache });
    const html = (await file.text()).replace('</body>', '<script src="/__reload.js"></script></body>');
    return new Response(html, { headers: { ...noCache, 'Content-Type': 'text/html; charset=utf-8' } });
  },
  websocket: {
    open(socket) { socket.subscribe('reload'); },
    message() {},
    idleTimeout: 0,
  },
});

watch(root, { recursive: true }, () => server.publish('reload', 'reload'));
console.log(`harbor frontend at ${server.url}`);
