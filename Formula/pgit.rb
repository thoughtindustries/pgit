class Pgit < Formula
  desc "Per-branch Postgres databases"
  homepage "https://github.com/thoughtindustries/pgit"
  url "https://github.com/thoughtindustries/pgit.git",
      using:    :git,
      tag:      "v0.2.0",
      revision: "3e35e4c0ca0a813341d6a891ddb539a270295490"
  license "GPL-3.0-or-later"
  head "https://github.com/thoughtindustries/pgit.git", using: :git, branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/pgit --version")
  end
end
