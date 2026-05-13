#!/usr/bin/env ruby
# frozen_string_literal: true

require "find"
require "open3"
require "pathname"
require "set"

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
  "crates/calciforge/src/commands.rs" => 3469,
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

# These counts are not style rules. They are ratchets for patterns that tend to
# grow accidental architecture surface area: shared mutable state, stringly
# typed maps, unjoined background tasks, and unsafe code. If a future PR needs
# more of one, bump the budget in that PR so reviewers see the tradeoff.
WATCH_PATTERN_BUDGETS = {
  "Arc<Mutex" => 40,
  "Arc<RwLock" => 11,
  "HashMap<String, String>" => 70,
  "tokio::spawn" => 141,
  "unsafe block" => 15
}.freeze

BASE_REF = ENV.fetch("CALCIFORGE_ARCH_RATCHET_BASE", "").strip

def git_output(*args)
  stdout, stderr, status = Open3.capture3("git", "-C", ROOT.to_s, *args)
  unless status.success?
    warn "architecture ratchet: could not run git #{args.join(' ')}: #{stderr.strip}"
    return nil
  end

  stdout
end

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

def load_base_rust_texts(base_ref)
  paths = git_output("ls-tree", "-r", "--name-only", base_ref, "crates")
  return {} unless paths

  paths.lines.each_with_object({}) do |line, texts|
    rel = line.strip
    next unless rel.end_with?(".rs")

    text = git_output("show", "#{base_ref}:#{rel}")
    next unless text

    texts[rel] = text
  end
end

def changed_files_since(base_ref)
  paths = git_output("diff", "--name-only", "--diff-filter=ACDMRT", "#{base_ref}...HEAD")
  return Set.new unless paths

  paths.lines.map(&:strip).reject(&:empty?).to_set
end

def count_patterns(texts)
  counts = Hash.new(0)
  texts.each_value do |text|
    WATCH_PATTERNS.each do |name, regex|
      counts[name] += text.scan(regex).count
    end
  end
  counts
end

failed = false
pattern_counts = Hash.new(0)
base_texts = {}
base_pattern_counts = Hash.new(0)
changed_files = Set.new
compare_to_base = false

unless BASE_REF.empty?
  base_texts = load_base_rust_texts(BASE_REF)
  changed_files = changed_files_since(BASE_REF)
  compare_to_base = !base_texts.empty?
  base_pattern_counts = count_patterns(base_texts) if compare_to_base
end

rust_files.each do |file|
  rel = relative(file)
  text = file.read
  line_count = text.lines.count
  budget = RUST_LINE_BUDGETS.fetch(rel, MAX_NEW_RUST_LINES)
  changed_in_pr = changed_files.include?(rel)
  base_line_count = base_texts.fetch(rel, "").lines.count

  if compare_to_base && RUST_LINE_BUDGETS.key?(rel)
    if changed_in_pr && line_count > budget && line_count > base_line_count
      warn "#{rel}: #{line_count} lines exceeds architecture budget #{budget} and grows from base #{base_line_count}"
      failed = true
    end
  elsif line_count > budget
    warn "#{rel}: #{line_count} lines exceeds architecture budget #{budget}"
    failed = true
  end

  stale_budget = RUST_LINE_BUDGETS.key?(rel) && line_count <= MAX_NEW_RUST_LINES
  stale_budget_changed = !compare_to_base || changed_in_pr
  if stale_budget && stale_budget_changed
    warn "#{rel}: #{line_count} lines no longer needs pinned architecture budget #{budget}"
    failed = true
  end

  WATCH_PATTERNS.each do |name, regex|
    pattern_counts[name] += text.scan(regex).count
  end
end

missing_budget_files = RUST_LINE_BUDGETS.keys.reject { |path| ROOT.join(path).file? }
if compare_to_base
  missing_budget_files.select! { |path| changed_files.include?(path) }
end
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

WATCH_PATTERN_BUDGETS.each do |name, budget|
  count = pattern_counts.fetch(name, 0)
  base_count = base_pattern_counts.fetch(name, 0)
  next unless count > budget

  if compare_to_base && count <= base_count
    next
  end

  message = "#{name}: #{count} occurrences exceeds architecture budget #{budget}"
  message += " and grows from base #{base_count}" if compare_to_base
  warn message
  failed = true
end

abort("architecture ratchets failed") if failed
