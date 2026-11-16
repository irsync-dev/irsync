class Irsync < Formula
  desc "irsync is a one-way directory synchroniser in the mould of rsync, rebuilt for today's networks."
  homepage "https://irsync.org"
  version "0.1.0-beta.3"
  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/irsync-dev/irsync/releases/download/v0.1.0-beta.3/irsync-aarch64-apple-darwin.tar.xz"
    end
  end
  if OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/irsync-dev/irsync/releases/download/v0.1.0-beta.3/irsync-aarch64-unknown-linux-musl.tar.xz"
    end
    if Hardware::CPU.intel?
      url "https://github.com/irsync-dev/irsync/releases/download/v0.1.0-beta.3/irsync-x86_64-unknown-linux-musl.tar.xz"
    end
  end
  license "MIT"

  BINARY_ALIASES = {
    "aarch64-apple-darwin": {},
    "aarch64-unknown-linux-gnu": {},
    "aarch64-unknown-linux-musl-dynamic": {},
    "aarch64-unknown-linux-musl-static": {},
    "x86_64-unknown-linux-gnu": {},
    "x86_64-unknown-linux-musl-dynamic": {},
    "x86_64-unknown-linux-musl-static": {}
  }

  def target_triple
    cpu = Hardware::CPU.arm? ? "aarch64" : "x86_64"
    os = OS.mac? ? "apple-darwin" : "unknown-linux-gnu"

    "#{cpu}-#{os}"
  end

  def install_binary_aliases!
    BINARY_ALIASES[target_triple.to_sym].each do |source, dests|
      dests.each do |dest|
        bin.install_symlink bin/source.to_s => dest
      end
    end
  end

  def install
    if OS.mac? && Hardware::CPU.arm?
      bin.install "irsync", "fileset"
    end
    if OS.linux? && Hardware::CPU.arm?
      bin.install "irsync", "fileset"
    end
    if OS.linux? && Hardware::CPU.intel?
      bin.install "irsync", "fileset"
    end

    install_binary_aliases!

    # Homebrew will automatically install these, so we don't need to do that
    doc_files = Dir["README.*", "readme.*", "LICENSE", "LICENSE.*", "CHANGELOG.*"]
    leftover_contents = Dir["*"] - doc_files

    # Install any leftover files in pkgshare; these are probably config or
    # sample files.
    pkgshare.install(*leftover_contents) unless leftover_contents.empty?
  end
end
