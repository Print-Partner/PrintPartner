// The test prepends `serve(server, path)` from the platform `test_support::SERVE`.
const fs = require("node:fs");
const http = require("node:http");
const readline = require("node:readline");

const input = readline.createInterface({ input: process.stdin });
process.on("SIGTERM", () => {});
input.once("line", (line) => {
  const setup = JSON.parse(line);
  const events = `${setup.data_dir}/upload-events`;
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
    let body = "";
    request.on("data", (chunk) => { body += chunk.toString(); });
    request.on("end", () => {
      fs.appendFileSync(events, `${request.method} ${request.url} ${body}\n`);
      // The effect is accepted, but its response is deliberately never sent.
    });
  }), setup.socket_path);
});
