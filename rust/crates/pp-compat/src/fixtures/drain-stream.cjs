// The test prepends `serve(server, path)` from the platform `test_support::SERVE`.
const fs = require("node:fs");
const http = require("node:http");
const readline = require("node:readline");

const input = readline.createInterface({ input: process.stdin });
process.on("SIGTERM", () => {});
input.once("line", (line) => {
  const setup = JSON.parse(line);
  const events = `${setup.data_dir}/stream-events`;
  input.once("close", () => {
    fs.appendFileSync(events, "EOF\n");
    process.exit(0);
  });
  serve(http.createServer((request, response) => {
    if (request.url === "/health") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({
        ok: true,
        version: "ticket-t-2-unreleased",
        release: { commit: "ticket-t-2-unreleased" },
      }));
      return;
    }
    fs.appendFileSync(events, `${request.method} ${request.url}\n`);
    response.setHeader("Content-Type", "application/octet-stream");
    response.write(Buffer.alloc(1024 * 1024, 120));
  }), setup.socket_path);
});
