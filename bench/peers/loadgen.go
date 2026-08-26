// A load generator that is not the bottleneck.
//
// `bench/load.py` runs 48 Python processes, one connection each. That was
// enough to measure a server answering a hundred thousand requests a second
// and is not enough to measure one answering a million: pointed at
// `bench/floor` it reports 770k at 48 connections, 1.49M at 96 and 2.51M at
// 160, which is the shape of a client running out of capacity rather than a
// server reaching one. Every fast number this repository has published was
// taken at 48 and is therefore a measurement of the harness.
//
// This is the same protocol — one connection per worker, held open, request
// and answer in lockstep — with goroutines instead of processes.
//
//	go run bench/peers/loadgen.go -port 18952 -conns 128 -seconds 5
package main

import (
	"bytes"
	"flag"
	"fmt"
	"net"
	"sync"
	"sync/atomic"
	"time"
)

func main() {
	port := flag.Int("port", 0, "the port to hammer")
	conns := flag.Int("conns", 128, "connections held open")
	seconds := flag.Int("seconds", 5, "how long to run")
	label := flag.String("label", "server", "what to call it")
	flag.Parse()

	request := []byte("GET /health HTTP/1.1\r\nHost: x\r\n\r\n")
	terminator := []byte("\r\n\r\n")

	var answered atomic.Int64
	var ready sync.WaitGroup
	var done sync.WaitGroup
	ready.Add(*conns)
	done.Add(*conns)
	start := make(chan struct{})

	for i := 0; i < *conns; i++ {
		go func() {
			defer done.Done()
			socket, err := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", *port), 10*time.Second)
			if err != nil {
				ready.Done()
				return
			}
			defer socket.Close()
			if tcp, ok := socket.(*net.TCPConn); ok {
				tcp.SetNoDelay(true)
			}
			ready.Done()
			<-start

			deadline := time.Now().Add(time.Duration(*seconds) * time.Second)
			// **Set once, not per request.** A deadline is two syscalls, and
			// setting one before every round trip cost more than the round
			// trip did: the first version of this reported a third of what
			// `load.py` reported for the same server, which is how it was
			// found. Generous, because it is a stuck-connection guard rather
			// than a per-request timeout.
			socket.SetDeadline(deadline.Add(30 * time.Second))
			buffer := make([]byte, 65536)
			var held []byte
			mine := int64(0)
			for time.Now().Before(deadline) {
				if _, err := socket.Write(request); err != nil {
					break
				}
				// Read to the end of the headers, as `load.py` does: every
				// server here answers in one write, so this turns once — it
				// exists so that one which does not is counted honestly.
				held = held[:0]
				for !bytes.Contains(held, terminator) {
					n, err := socket.Read(buffer)
					if err != nil || n == 0 {
						break
					}
					held = append(held, buffer[:n]...)
				}
				if !bytes.Contains(held, terminator) {
					break
				}
				mine++
			}
			answered.Add(mine)
		}()
	}

	ready.Wait()
	began := time.Now()
	close(start)
	done.Wait()
	elapsed := time.Since(began).Seconds()

	total := answered.Load()
	fmt.Printf("  %-28s %9.0f req/s  (%d in %.1fs, %d conns)\n",
		*label, float64(total)/elapsed, total, elapsed, *conns)
}
