#!/usr/bin/env ruby
# frozen_string_literal: true

require "find"
require "pathname"

ROOT = Pathname.new(__dir__).join("..").expand_path
MAX_NEW_RUST_LINES = 700

# Existing large files are technical debt, not precedent. Their budgets are
# pinned to the line counts from the first architecture-ratchet pass; growing
# one of these files should be an explicit decision, preferably paired with a
# split or a tighter follow-up budget.
RUST_LINE_BUDGETS = {
  "crates/adversary-detector/src/proxy.rs" => 730,
  "crates/adversary-detector/src/scanner.rs" => 1387,
  "crates/calciforge/src/adapters/codex_cli.rs" => 752,
  "crates/calciforge/src/adapters/mod.rs" => 1374,
  "crates/calciforge/src/adapters/openclaw_channel.rs" => 1768,
  "crates/calciforge/src/channels/matrix.rs" => 1815,
  "crates/calciforge/src/channels/mock.rs" => 804,
  "crates/calciforge/src/channels/signal.rs" => 1066,
  "crates/calciforge/src/channels/sms.rs" => 958,
  "crates/calciforge/src/channels/telegram.rs" => 2088,
  "crates/calciforge/src/channels/whatsapp.rs" => 1291,
  "crates/calciforge/src/commands.rs" => 4890,
  "crates/calciforge/src/config.rs" => 2365,
  "crates/calciforge/src/config/validator.rs" => 2419,
  "crates/calciforge/src/doctor.rs" => 3580,
  "crates/calciforge/src/install/cli.rs" => 1071,
  "crates/calciforge/src/install/executor.rs" => 3681,
  "crates/calciforge/src/install/linux_hardening.rs" => 751,
  "crates/calciforge/src/install/model.rs" => 819,
  "crates/calciforge/src/install/ssh.rs" => 1124,
  "crates/calciforge/src/install/wizard.rs" => 701,
  "crates/calciforge/src/providers/alloy.rs" => 1126,
  "crates/calciforge/src/proxy/gateway.rs" => 1001,
  "crates/calciforge/src/proxy/handlers.rs" => 2386,
  "crates/host-agent/src/main.rs" => 1288,
  "crates/paste-server/src/lib.rs" => 2623,
  "crates/security-proxy/src/mitm.rs" => 1573,
  "crates/security-proxy/src/proxy.rs" => 2296,
  "crates/security-proxy/src/substitution.rs" => 912,
  "crates/secrets-client/src/fnox_client.rs" => 787
}.freeze

WATCH_PATTERNS = {
  "Arc<Mutex" => /Arc<Mutex/,
  "Arc<RwLock" => /Arc<RwLock/,
  "HashMap<String, String>" => /HashMap\s*<\s*String\s*,\s*String\s*>/,
  "Vec<String>" => /Vec\s*<\s*String\s*>/,
  "tokio::spawn" => /tokio::spawn\s*\(/,
  "positional indexing" => /\[[0-9]+\]/,
  "unsafe block" => /unsafe\s*\{/
}.freeze

def rust_files
  files = []
  Find.find(ROOT.join("crates").to_s) do |path|
    next unless path.end_with?(".rs")
    next if path.include?("/target/")

    files << Pathname.new(path)
  end
  files.sort
end

def relative(path)
  path.relative_path_from(ROOT).to_s
end

failed = false
pattern_counts = Hash.new(0)

rust_files.each do |file|
  rel = relative(file)
  text = file.read
  line_count = text.lines.count
  budget = RUST_LINE_BUDGETS.fetch(rel, MAX_NEW_RUST_LINES)

  if line_count > budget
    warn "#{rel}: #{line_count} lines exceeds architecture budget #{budget}"
    failed = true
  end

  WATCH_PATTERNS.each do |name, regex|
    pattern_counts[name] += text.scan(regex).count
  end
end

missing_budget_files = RUST_LINE_BUDGETS.keys.reject { |path| ROOT.join(path).file? }
unless missing_budget_files.empty?
  warn "architecture budget references missing files:"
  missing_budget_files.each { |path| warn "  #{path}" }
  failed = true
end

puts "Architecture ratchets:"
puts "  max lines for new Rust modules: #{MAX_NEW_RUST_LINES}"
puts "  pinned large-module budgets: #{RUST_LINE_BUDGETS.length}"
puts "  watched pattern counts:"
pattern_counts.sort.each do |name, count|
  puts "    #{name}: #{count}"
end

abort("architecture ratchets failed") if failed
