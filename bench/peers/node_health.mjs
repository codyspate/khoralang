// Node's `http`, answering what `bench/service` answers.
import { createServer } from "node:http";

const port = Number(process.argv[2]);
const body = '{"status":"ok"}';

createServer((request, answer) => {
  if (request.url === "/health") {
    answer.writeHead(200, { "Content-Type": "application/json" });
    answer.end(body);
  } else {
    answer.writeHead(404);
    answer.end();
  }
}).listen(port, "127.0.0.1", () => console.log("listening on " + port));
