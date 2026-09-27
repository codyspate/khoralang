-- Prints one JSON line when a wrk run ends: what run.py reads.
-- Latency is in microseconds; wrk measures every request, not a sample.
done = function(summary, latency, requests)
  local e = summary.errors
  io.write(string.format(
    '{"requests":%d,"duration_us":%d,"bytes":%d,"p50_us":%d,"p99_us":%d,"max_us":%d,' ..
    '"connect_errors":%d,"read_errors":%d,"write_errors":%d,"timeouts":%d,"non_2xx":%d}\n',
    summary.requests, summary.duration, summary.bytes,
    latency:percentile(50), latency:percentile(99), latency.max,
    e.connect, e.read, e.write, e.timeout, e.status))
end
