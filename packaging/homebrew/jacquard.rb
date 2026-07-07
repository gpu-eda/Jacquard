# frozen_string_literal: true

# Homebrew formula for Jacquard (macOS / Metal prebuilt binary).
#
# This is the SOURCE-OF-TRUTH TEMPLATE for the formula. Apple Silicon + Metal
# only — the simulator needs a Metal GPU.
#
# The `url`/`version`/`sha256` below are placeholders: on every release tag,
# the `bump-tap` job in `.github/workflows/release.yml` rewrites them from the
# published tarball + `.sha256` and pushes the result to the tap —
# `gpu-eda/homebrew-tap` for final releases, `gpu-eda/homebrew-tap-prerelease`
# for RCs. So this file need not be bumped by hand; edit it only to change the
# formula *structure* (deps, install steps, test), not the version pin.
class Jacquard < Formula
  desc "GPU-accelerated RTL logic simulator (Metal backend)"
  homepage "https://github.com/gpu-eda/Jacquard"
  url "https://github.com/gpu-eda/Jacquard/releases/download/v0.2.3/jacquard-0.2.3-macos-arm64-metal.tar.gz"
  version "0.2.3"
  sha256 "814ba9cb1b74c83f5471e6cb6f992254e704ca5c894568a5b8c19903a7a99e4f"
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
