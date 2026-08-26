// The JDK's built-in HTTP server, answering what `bench/service` answers.
//
// Labelled carefully in the table: `com.sun.net.httpserver` ships with the JDK
// and is not what a production Java service runs on. It is here because it is
// the only Java server available without fetching a framework, and a number
// with a caveat beats an empty row.
import com.sun.net.httpserver.HttpServer;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.Executors;

public class JavaHealth {
  public static void main(String[] args) throws Exception {
    int port = Integer.parseInt(args[0]);
    byte[] body = "{\"status\":\"ok\"}".getBytes(StandardCharsets.UTF_8);
    HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", port), 511);
    server.createContext("/health", exchange -> {
      exchange.getResponseHeaders().set("Content-Type", "application/json");
      exchange.sendResponseHeaders(200, body.length);
      try (OutputStream out = exchange.getResponseBody()) {
        out.write(body);
      }
    });
    server.setExecutor(Executors.newFixedThreadPool(Runtime.getRuntime().availableProcessors()));
    server.start();
    System.out.println("listening on " + port);
  }
}
