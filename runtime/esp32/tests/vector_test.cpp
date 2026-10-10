#include <cassert>
#include <cstdint>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

static std::string read_file(const std::string& path) {
  std::ifstream in(path);
  std::ostringstream out;
  out << in.rdbuf();
  return out.str();
}

static std::string json_hex(const std::string& json) {
  const auto key = json.find("\"hex\"");
  assert(key != std::string::npos);
  const auto first = json.find('"', key + 5);
  const auto second = json.find('"', first + 1);
  assert(first != std::string::npos && second != std::string::npos);
  return json.substr(first + 1, second - first - 1);
}

static std::vector<std::uint8_t> parse_hex(const std::string& hex) {
  std::vector<std::uint8_t> out;
  for (std::size_t i = 0; i + 1 < hex.size(); i += 2) {
    out.push_back(static_cast<std::uint8_t>(std::stoul(hex.substr(i, 2), nullptr, 16)));
  }
  return out;
}

static std::uint16_t u16le(const std::uint8_t* p) {
  return static_cast<std::uint16_t>(p[0] | (p[1] << 8));
}

int main(int argc, char** argv) {
  const std::string dir = argc > 1 ? argv[1] : "protocol/vectors";
  const auto legacy = parse_hex(json_hex(read_file(dir + "/frame_legacy.json")));
  assert(legacy[0] == 0);
  assert(legacy[1] == 4);
  assert(std::string(reinterpret_cast<const char*>(legacy.data() + 2), 4) == "main");
  assert(u16le(legacy.data() + 2 + 4 + 8) == 2);
  assert(u16le(legacy.data() + 2 + 4 + 8 + 2) == 2);

  const auto begin = parse_hex(json_hex(read_file(dir + "/frame_begin.json")));
  assert(begin[0] == 1);
  const auto chunk = parse_hex(json_hex(read_file(dir + "/frame_chunk.json")));
  assert(chunk[0] == 2);
  const auto commit = parse_hex(json_hex(read_file(dir + "/frame_commit.json")));
  assert(commit[0] == 3);

  const auto tile = parse_hex(json_hex(read_file(dir + "/tile.json")));
  assert(tile[0] == 4);
  assert(std::string(reinterpret_cast<const char*>(tile.data() + 1), 4) == "main");
  assert(tile[tile.size() - 2] == 0xaa);
  assert(tile.back() == 0xbb);
  return 0;
}
