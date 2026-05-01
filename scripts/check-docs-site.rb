#!/usr/bin/env ruby
# frozen_string_literal: true

require "find"
require "pathname"
require "uri"

ROOT = Pathname.new(__dir__).join("..").expand_path
DOCS = ROOT.join("docs")

def fail_with(message)
  warn message
  $failed = true
end

def markdown_files
  files = []
  Find.find(DOCS.to_s) do |path|
    next unless path.end_with?(".md")

    files << Pathname.new(path)
  end
  files.sort
end

def external_href?(href)
  uri = URI.parse(href)
  uri.scheme || href.start_with?("//", "mailto:")
rescue URI::InvalidURIError
  false
end

def generated_page_source(target)
  return nil unless target.extname == ".html"

  target.sub_ext(".md")
end

def validate_front_matter(file)
  text = file.read
  return if text.start_with?("---\n")

  fail_with("#{file.relative_path_from(ROOT)}: missing front matter; docs site pages must render as HTML")
end

def validate_href(file, href)
  href = href.strip
  return if href.empty? || href.start_with?("#") || external_href?(href)

  path_part = href.split("#", 2).first
  return if path_part.empty?
  return if path_part.start_with?("/")

  if path_part.end_with?(".md")
    fail_with("#{file.relative_path_from(ROOT)}: links to source Markdown instead of generated HTML: #{href}")
    return
  end

  target = (file.dirname + path_part).cleanpath
  if path_part.end_with?(".html")
    source = generated_page_source(target)
    return if source && source.file?

    fail_with("#{file.relative_path_from(ROOT)}: missing docs page for link #{href}")
    return
  end

  return if target.file? || target.directory?

  fail_with("#{file.relative_path_from(ROOT)}: missing local link target #{href}")
end

markdown_files.each do |file|
  validate_front_matter(file)
  text = file.read

  text.scan(/\[[^\]]+\]\(([^)]+)\)/) do |(href)|
    validate_href(file, href)
  end

  text.scan(/href=["']([^"']+)["']/) do |(href)|
    validate_href(file, href)
  end
end

abort("docs site checks failed") if $failed

puts "docs site checks passed"
