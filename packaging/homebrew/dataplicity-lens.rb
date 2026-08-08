# typed: strict
# frozen_string_literal: true

# Homebrew formula for the complete Dataplicity Lens command suite.
class DataplicityLens < Formula
  desc "Local system operations toolkit for Linux and macOS"
  homepage "https://lens.dataplicity.com/"
  url "https://github.com/wildfoundry/dataplicity-lens.git", branch: "main"
  version "0.3.0"
  license "Apache-2.0"

  depends_on "rust" => :build

  def install
    packages = %w[lens lens-top lens-services lens-containers lens-logs lens-disk lens-net lens-hardware lens-system lens-health]
    packages.each do |package|
      system "cargo", "install", *std_cargo_args(path: "apps/#{package}")
    end

    system "scripts/generate-assets.sh", bin/"lens-top", "dist/generated"
    man1.install Dir["dist/generated/man/*.1"]
    bash_completion.install Dir["dist/generated/completions/*.bash"]
    zsh_completion.install Dir["dist/generated/completions/_*"]
    fish_completion.install Dir["dist/generated/completions/*.fish"]
  end

  test do
    commands = %w[lens lens-top lens-services lens-containers lens-logs lens-disk lens-net lens-hardware lens-system lens-health]
    commands.each do |command|
      output = shell_output("#{bin}/#{command} --demo --json")
      assert_match '"schema_version": "2"', output
    end

    if ENV["LENS_HOMEBREW_SKIP_NATIVE"] != "1"
      native = shell_output("#{bin}/lens-top --once --json")
      assert_match '"hostname":', native
      assert_match '"processes":', native
    end
  end
end
