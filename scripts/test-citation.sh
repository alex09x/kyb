#!/usr/bin/env bash
# CITATION.cff is what Zenodo archives and what GitHub renders as "Cite this
# repository". A Zenodo deposition is immutable, so a stale version number or a
# file that stopped parsing is preserved permanently - and the site already
# shipped structured data claiming 0.1 against a 0.2.1 Cargo.toml, which is the
# same drift in a place where it was merely embarrassing rather than permanent.
#
#   scripts/test-citation.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[ -n "$cargo_version" ] || { echo "could not read version from Cargo.toml" >&2; exit 1; }

ruby -ryaml -e '
  cargo_version = ARGV[0]
  doc = YAML.safe_load(File.read("CITATION.cff"))

  %w[cff-version message title abstract type authors repository-code license version].each do |key|
    abort "CITATION.cff: missing required key: #{key}" unless doc.key?(key)
  end

  abort "CITATION.cff: cff-version must be 1.2.0, got #{doc["cff-version"]}" unless doc["cff-version"] == "1.2.0"
  abort "CITATION.cff: type must be software, got #{doc["type"]}" unless doc["type"] == "software"

  unless doc["version"].to_s == cargo_version
    abort "CITATION.cff: version #{doc["version"]} does not match Cargo.toml #{cargo_version}"
  end

  authors = doc["authors"]
  abort "CITATION.cff: authors must be a non-empty list" unless authors.is_a?(Array) && !authors.empty?
  authors.each do |a|
    next if a.key?("family-names") || a.key?("name")
    abort "CITATION.cff: an author needs family-names (person) or name (entity)"
  end

  # Not fatal: the record is still provisional until the author is a person with
  # an ORCID. It must be loud, because it has to be settled before a DOI exists.
  person = authors.any? { |a| a.key?("family-names") }
  orcid   = authors.any? { |a| a.key?("orcid") }
  unless person && orcid
    warn "CITATION.cff: still provisional - " \
         "#{person ? "" : "author is an entity, not a person; "}" \
         "#{orcid ? "" : "no ORCID; "}" \
         "settle both BEFORE minting a Zenodo DOI (the deposition is immutable)"
  end

  puts "CITATION.cff valid: #{doc["title"]} #{doc["version"]}, #{authors.length} author(s)"
' "$cargo_version"
