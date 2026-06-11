package main

import (
	"reflect"
	"testing"
)

func TestStringListPreservesInstrumentedStringSlices(t *testing.T) {
	input := []string{"/bin/sh", "-lc", "exec \"$@\"", "marker", "python3", "agent.py"}

	got := stringList(input)

	if !reflect.DeepEqual(got, input) {
		t.Fatalf("stringList(%#v) = %#v", input, got)
	}

	got[0] = "mutated"
	if input[0] != "/bin/sh" {
		t.Fatalf("stringList returned an alias of the input slice")
	}
}

func TestStringListReadsJSONDecodedArrays(t *testing.T) {
	input := []any{"/bin/sh", "-lc", 42, "echo ok"}

	got := stringList(input)
	want := []string{"/bin/sh", "-lc", "echo ok"}

	if !reflect.DeepEqual(got, want) {
		t.Fatalf("stringList(%#v) = %#v, want %#v", input, got, want)
	}
}
