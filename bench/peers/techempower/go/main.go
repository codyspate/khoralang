// The four TechEmpower read tests on Go's standard library plus pgx:
// net/http, encoding/json, html/template, jackc/pgx/v5 with pgxpool.
//
// What a Go team would write, and the same work as the Khora, Node and Bun
// servers beside it: a pool of 16, one query per row run one after another,
// no caching, the date and server headers on every answer.
package main

import (
	"context"
	"encoding/json"
	"html/template"
	"log"
	"math/rand/v2"
	"net/http"
	"os"
	"sort"
	"strconv"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// World is one row of the World table.
type World struct {
	ID           int32 `json:"id"`
	RandomNumber int32 `json:"randomNumber"`
}

// Fortune is one row of the Fortune table, or the one added per request.
type Fortune struct {
	ID      int32
	Message string
}

// The minimum template from the Fortunes rules, whitespace removed.
// html/template escapes {{.Message}} because it is text inside an element.
var fortunesPage = template.Must(template.New("fortunes").Parse(
	`<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>` +
		`{{range .}}<tr><td>{{.ID}}</td><td>{{.Message}}</td></tr>{{end}}` +
		`</table></body></html>`))

const worldQuery = "SELECT id, randomnumber FROM world WHERE id = $1"
const fortuneQuery = "SELECT id, message FROM fortune"

var pool *pgxpool.Pool

func setting(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}

// net/http writes Date itself; Server it leaves to the application.
func headers(w http.ResponseWriter, contentType string) {
	w.Header().Set("Server", "go")
	w.Header().Set("Content-Type", contentType)
}

func jsonHandler(w http.ResponseWriter, r *http.Request) {
	body, _ := json.Marshal(struct {
		Message string `json:"message"`
	}{"Hello, World!"})
	headers(w, "application/json")
	w.Header().Set("Content-Length", strconv.Itoa(len(body)))
	w.Write(body)
}

func randomWorld(ctx context.Context) (World, error) {
	var world World
	err := pool.QueryRow(ctx, worldQuery, rand.IntN(10000)+1).Scan(&world.ID, &world.RandomNumber)
	return world, err
}

func dbHandler(w http.ResponseWriter, r *http.Request) {
	world, err := randomWorld(r.Context())
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	body, _ := json.Marshal(world)
	headers(w, "application/json")
	w.Header().Set("Content-Length", strconv.Itoa(len(body)))
	w.Write(body)
}

// queriesOf applies TechEmpower's rule: missing, not an integer or below one
// is one; above 500 is 500.
func queriesOf(r *http.Request) int {
	n, err := strconv.Atoi(r.URL.Query().Get("queries"))
	if err != nil || n < 1 {
		return 1
	}
	if n > 500 {
		return 500
	}
	return n
}

func queriesHandler(w http.ResponseWriter, r *http.Request) {
	n := queriesOf(r)
	worlds := make([]World, 0, n)
	for i := 0; i < n; i++ {
		world, err := randomWorld(r.Context())
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		worlds = append(worlds, world)
	}
	body, _ := json.Marshal(worlds)
	headers(w, "application/json")
	w.Header().Set("Content-Length", strconv.Itoa(len(body)))
	w.Write(body)
}

// pipelinedHandler is the multiple-queries test with every lookup in one
// pgx batch: pool.SendBatch writes them all before reading a reply. In pgx's
// default mode each query is its own statement with its own Sync, which is
// what TechEmpower's rule 7 asks of pipelining. Same clamping and body as
// queriesHandler.
func pipelinedHandler(w http.ResponseWriter, r *http.Request) {
	n := queriesOf(r)
	worlds := make([]World, n)
	batch := &pgx.Batch{}
	for i := 0; i < n; i++ {
		batch.Queue(worldQuery, rand.IntN(10000)+1)
	}
	br := pool.SendBatch(r.Context(), batch)
	for i := 0; i < n; i++ {
		if err := br.QueryRow().Scan(&worlds[i].ID, &worlds[i].RandomNumber); err != nil {
			br.Close()
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
	}
	if err := br.Close(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	body, _ := json.Marshal(worlds)
	headers(w, "application/json")
	w.Header().Set("Content-Length", strconv.Itoa(len(body)))
	w.Write(body)
}

func fortunesHandler(w http.ResponseWriter, r *http.Request) {
	rows, err := pool.Query(r.Context(), fortuneQuery)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	fortunes := []Fortune{}
	for rows.Next() {
		var f Fortune
		if err := rows.Scan(&f.ID, &f.Message); err != nil {
			rows.Close()
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		fortunes = append(fortunes, f)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	fortunes = append(fortunes, Fortune{0, "Additional fortune added at request time."})
	sort.Slice(fortunes, func(i, j int) bool { return fortunes[i].Message < fortunes[j].Message })
	headers(w, "text/html; charset=utf-8")
	if err := fortunesPage.Execute(w, fortunes); err != nil {
		log.Print(err)
	}
}

func main() {
	url := "postgres://" + setting("PGUSER", "benchmarkdbuser") + ":" + setting("PGPASSWORD", "benchmarkdbpass") +
		"@" + setting("PGHOST", "127.0.0.1") + ":" + setting("PGPORT", "5432") + "/" + setting("PGDATABASE", "hello_world") +
		"?pool_max_conns=" + setting("POOL", "16") + "&pool_min_conns=" + setting("POOL", "16")
	config, err := pgxpool.ParseConfig(url)
	if err != nil {
		log.Fatal(err)
	}
	pool, err = pgxpool.NewWithConfig(context.Background(), config)
	if err != nil {
		log.Fatal(err)
	}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /json", jsonHandler)
	mux.HandleFunc("GET /db", dbHandler)
	mux.HandleFunc("GET /queries", queriesHandler)
	mux.HandleFunc("GET /pipelined-queries", pipelinedHandler)
	mux.HandleFunc("GET /fortunes", fortunesHandler)
	log.Fatal(http.ListenAndServe(":"+setting("PORT", "8080"), mux))
}
