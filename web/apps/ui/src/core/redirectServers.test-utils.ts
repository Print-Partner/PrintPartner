import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";

export async function redirectServers(landed: unknown) {
  const first: string[] = [];
  const destination: string[] = [];
  let status: 307 | 308 = 307;
  const listen = async (server: Server) => {
    await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
    return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  };
  const target = createServer((request, response) => {
    destination.push(`${request.method} ${request.url}`);
    request.resume();
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(landed));
  });
  const targetOrigin = await listen(target);
  const proxy = createServer((request, response) => {
    first.push(`${request.method} ${request.url}`);
    request.resume();
    response.writeHead(status, { location: `${targetOrigin}/landed` });
    response.end();
  });
  const origin = await listen(proxy);
  const nodeFetch = globalThis.fetch;
  return {
    first,
    destination,
    redirectWith(next: 307 | 308) {
      status = next;
      first.length = 0;
      destination.length = 0;
    },
    forward: (path: string, init?: RequestInit) => nodeFetch(`${origin}${path}`, init),
    close: () =>
      Promise.all(
        [proxy, target].map((server) => new Promise<void>((done) => server.close(() => done()))),
      ),
  };
}
