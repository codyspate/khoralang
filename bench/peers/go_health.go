// Go's `net/http`, answering what `bench/service` answers.
//
// The idiomatic server rather than a hand-rolled socket loop, because the
// comparison worth making is against what a team would actually write.
package main

import (
	"fmt"
	"net/http"
	"os"
)

func main() {
	port := os.Args[1]
	body := []byte(`{"status":"ok"}`)
	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write(body)
	})
	fmt.Println("listening on " + port)
	http.ListenAndServe("127.0.0.1:"+port, nil)
}
