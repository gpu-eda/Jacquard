# frozen_string_literal: true

# Homebrew formula for Jacquard (macOS / Metal prebuilt binary).
#
# This is the SOURCE OF TRUTH for the formula; it is copied/PR'd into the
# `gpu-eda/homebrew-tap` repo as `Formula/jacquard.rb` (see
# packaging/README.md). Apple Silicon + Metal only — the simulator needs a
# Metal GPU.
#
# Per release, bump `url` (both version occurrences), `version`, and
# `sha256` to the `jacquard-<version>-macos-arm64-metal.tar.gz` asset. The
# release workflow emits the `.sha256` alongside the tarball; bump by hand,
# with `brew bump-formula-pr`, or via a future release-CI step.
class Jacquard < Formula
  desc "GPU-accelerated RTL logic simulator (Metal backend)"
  homepage "https://github.com/gpu-eda/Jacquard"
  url "https://github.com/gpu-eda/Jacquard/releases/download/v0.2.1/jacquard-0.2.1-macos-arm64-metal.tar.gz"
  version "0.2.1"
  sha256 "230fe1e0569cd463ab2be1e700de8284b0e101dfd8a0a4aeddeb6fba746cfba0"
  license "Apache-2.0"

  depends_on arch: :arm64
  depends_on :macos

  def install
    bin.install "jacquard"
    bin.install "opensta-to-ir"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/jacquard --version")
  end
end
