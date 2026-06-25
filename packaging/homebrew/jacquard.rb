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
  url "https://github.com/gpu-eda/Jacquard/releases/download/v0.2.2/jacquard-0.2.2-macos-arm64-metal.tar.gz"
  version "0.2.2"
  sha256 "38e3c8954e4e3e04c1f8a0b541b8f165aada7ae0d38d3e8d2e9c8d9ef5a8c550"
  license "Apache-2.0"

  depends_on arch: :arm64
  # The prebuilt binary links Homebrew LLVM's libc++ and libomp (the build
  # uses LLVM clang for OpenMP, via the mt-kahypar partitioner). Declaring the
  # dependency makes `brew install` pull LLVM so the binary loads on a clean
  # machine. (binstall / raw-tarball users must `brew install llvm` themselves
  # — see docs/installation.md.)
  depends_on "llvm"
  depends_on :macos

  def install
    bin.install "jacquard"
    bin.install "timing_analysis"
    bin.install "opensta-to-ir"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/jacquard --version")
  end
end
