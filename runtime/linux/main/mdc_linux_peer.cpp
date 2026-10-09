/**
 * C++ linux-virt TCP peer — SessionDispatcher + SimDisplay over TCP.
 * Host: `mdc pair sim-linux-virt <token>` then `mdc --address 127.0.0.1:9877 send-image …`
 *
 * Usage: mdc-linux-peer [host:port] [width] [height]
 * Default: 127.0.0.1:9877 800 480
 */
#include "linux_runtime.hpp"

#include <arpa/inet.h>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <netinet/in.h>
#include <string>
#include <sys/socket.h>
#include <unistd.h>

namespace {

bool write_all(int fd, const std::uint8_t* data, std::size_t len) {
  std::size_t off = 0;
  while (off < len) {
    const auto n = ::write(fd, data + off, len - off);
    if (n < 0) {
      if (errno == EINTR) continue;
      return false;
    }
    if (n == 0) return false;
    off += static_cast<std::size_t>(n);
  }
  return true;
}

void flush_replies(int client, mdc::LinuxRuntime& runtime) {
  for (const auto& reply : runtime.replies()) {
    auto bytes = mdc::encode_packet(reply.type, reply.request_id, reply.payload);
    if (!write_all(client, bytes.data(), bytes.size())) {
      return;
    }
  }
  runtime.clear_replies();
}

void drain_parser(int client, mdc::Parser& parser, mdc::LinuxRuntime& runtime) {
  for (;;) {
    mdc::Packet packet;
    const auto pr = parser.feed(nullptr, 0, packet);
    if (pr != mdc::ParseResult::Packet) break;
    runtime.handle_packet(packet);
    flush_replies(client, runtime);
  }
}

void serve_client(int client, mdc::LinuxRuntime& runtime) {
  mdc::Parser parser;
  std::uint8_t buf[8192];
  for (;;) {
    const auto n = ::read(client, buf, sizeof(buf));
    if (n == 0) break;
    if (n < 0) {
      if (errno == EINTR) continue;
      break;
    }
    mdc::Packet packet;
    const auto pr = parser.feed(buf, static_cast<std::size_t>(n), packet);
    if (pr == mdc::ParseResult::Invalid) {
      std::fprintf(stderr, "mdc-linux-peer: parse invalid\n");
      return;
    }
    if (pr == mdc::ParseResult::Packet) {
      runtime.handle_packet(packet);
      flush_replies(client, runtime);
      drain_parser(client, parser, runtime);
    }
  }
}

}  // namespace

int main(int argc, char** argv) {
  std::string host = "127.0.0.1";
  int port = 9877;
  std::uint16_t width = 800;
  std::uint16_t height = 480;

  if (argc >= 2) {
    std::string arg = argv[1];
    const auto colon = arg.rfind(':');
    if (colon != std::string::npos) {
      if (colon > 0) host = arg.substr(0, colon);
      port = std::atoi(arg.substr(colon + 1).c_str());
    } else {
      port = std::atoi(arg.c_str());
    }
  }
  if (argc >= 4) {
    width = static_cast<std::uint16_t>(std::atoi(argv[2]));
    height = static_cast<std::uint16_t>(std::atoi(argv[3]));
  }
  if (port <= 0 || width == 0 || height == 0) {
    std::fprintf(stderr, "usage: mdc-linux-peer [host:port] [width] [height]\n");
    return 2;
  }

  mdc::PeerIdentity identity;
  identity.device_id = "sim-linux-virt";
  identity.firmware = "0.1.0-linux";
  identity.touch = false;
  identity.ota = false;

  int server = ::socket(AF_INET, SOCK_STREAM, 0);
  if (server < 0) {
    std::perror("socket");
    return 1;
  }
  int yes = 1;
  ::setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &yes, sizeof(yes));

  sockaddr_in addr{};
  addr.sin_family = AF_INET;
  addr.sin_port = htons(static_cast<std::uint16_t>(port));
  if (::inet_pton(AF_INET, host.c_str(), &addr.sin_addr) != 1) {
    std::fprintf(stderr, "bad host %s\n", host.c_str());
    ::close(server);
    return 1;
  }
  if (::bind(server, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) < 0) {
    std::perror("bind");
    ::close(server);
    return 1;
  }
  if (::listen(server, 1) < 0) {
    std::perror("listen");
    ::close(server);
    return 1;
  }

  std::printf("mdc-linux-peer listening on %s:%d size=%ux%u device_id=%s\n", host.c_str(), port,
              width, height, identity.device_id.c_str());

  for (;;) {
    sockaddr_in peer{};
    socklen_t peer_len = sizeof(peer);
    int client = ::accept(server, reinterpret_cast<sockaddr*>(&peer), &peer_len);
    if (client < 0) {
      if (errno == EINTR) continue;
      std::perror("accept");
      break;
    }
    std::printf("peer connected\n");
    mdc::LinuxRuntime runtime(width, height, identity);
    serve_client(client, runtime);
    ::close(client);
    std::printf("peer disconnected fb_bytes=%zu\n", runtime.framebuffer().size());
  }
  ::close(server);
  return 0;
}
