class Pgit < Formula
  desc "Per-branch Postgres databases"
  homepage "https://github.com/thoughtindustries/pgit"
  url "https://github.com/thoughtindustries/pgit.git",
      using:    :git,
      tag:      "v0.3.0",
      revision: "fea3d23bc0a5ebd52ce7bcd87cbd14918fb7a913"
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
