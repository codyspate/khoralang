// Reads a scrape with Prometheus' own Go parsers and fails on anything they
// refuse.
//
// Two parsers, because they refuse different things. `expfmt.TextParser` is
// what `promtool check metrics` and client_golang use: it refuses a second
// `# TYPE` for one family and a family split across the text. The scrape
// loop of the Prometheus server uses `textparse.PromParser`, which accepts a
// series twice and then keeps one of the two samples: this reports that as a
// failure, because the server would drop one of them on every scrape.
//
// Exit status 0 and one line per series on stdout, sorted, as
// `name{labels} value`; anything else is exit status 1 and the reason.
package main

import (
	"errors"
	"fmt"
	"io"
	"os"
	"sort"
	"strconv"
	"strings"

	"github.com/prometheus/common/expfmt"
	"github.com/prometheus/common/model"
	"github.com/prometheus/prometheus/model/labels"
	"github.com/prometheus/prometheus/model/textparse"
)

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
	os.Exit(1)
}

func main() {
	if len(os.Args) != 2 {
		fail("usage: prometheus_go <scrape file>")
	}
	data, err := os.ReadFile(os.Args[1])
	if err != nil {
		fail("reading the scrape: %v", err)
	}

	parser := expfmt.NewTextParser(model.LegacyValidation)
	if _, err := parser.TextToMetricFamilies(strings.NewReader(string(data))); err != nil {
		fail("expfmt: %v", err)
	}

	scrape := textparse.NewPromParser(data, labels.NewSymbolTable(), false)
	seen := map[string]bool{}
	var lines []string
	for {
		entry, err := scrape.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			fail("textparse: %v", err)
		}
		if entry != textparse.EntrySeries {
			continue
		}
		var series labels.Labels
		scrape.Labels(&series)
		_, _, value := scrape.Series()
		key := series.String()
		if seen[key] {
			fail("textparse: the series %s appears twice", key)
		}
		seen[key] = true
		lines = append(lines, key+" "+strconv.FormatFloat(value, 'g', -1, 64))
	}
	sort.Strings(lines)
	for _, line := range lines {
		fmt.Println(line)
	}
}
