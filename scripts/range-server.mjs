import { createReadStream, statSync } from "node:fs";
import { createServer } from "node:http";
import { basename, resolve } from "node:path";

const file = resolve(process.argv[2] ?? "range-test.bin");
const port = Number(process.argv[3] ?? 8765);
const fileName = basename(file);
const { size } = statSync(file);

const server = createServer((request, response) => {
  if (request.url !== "/" + encodeURIComponent(basename(file))) {
    response.writeHead(404);
    response.end();
    return;
  }

  const range = request.headers.range;
  response.setHeader("Accept-Ranges", "bytes");
  response.setHeader("Content-Type", "application/octet-stream");
  response.setHeader("Content-Disposition", 'attachment; filename="' + fileName.replaceAll('"', "") + '"');

  if (request.method === "HEAD") {
    response.setHeader("Content-Length", size);
    response.writeHead(200);
    response.end();
    return;
  }

  if (!range) {
    response.setHeader("Content-Length", size);
    response.writeHead(200);
    createReadStream(file).pipe(response);
    return;
  }

  const match = /^bytes=(\d+)-(\d*)$/.exec(range);
  if (!match) {
    response.setHeader("Content-Range", "bytes */" + size);
    response.writeHead(416);
    response.end();
    return;
  }

  const start = Number(match[1]);
  const end = Math.min(match[2] ? Number(match[2]) : size - 1, size - 1);
  if (start >= size || end < start) {
    response.setHeader("Content-Range", "bytes */" + size);
    response.writeHead(416);
    response.end();
    return;
  }

  response.setHeader("Content-Length", end - start + 1);
  response.setHeader("Content-Range", "bytes " + start + "-" + end + "/" + size);
  response.writeHead(206);
  createReadStream(file, { start, end }).pipe(response);
});

server.listen(port, "127.0.0.1", () => {
  process.stdout.write(
    "Serving " + file + " at http://127.0.0.1:" + port + "/" + encodeURIComponent(basename(file)) + "\n",
  );
});
