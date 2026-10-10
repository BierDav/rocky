"""A symmetric localhost UDP route to one Android emulator listener."""
import argparse
import socket
import subprocess
import threading


class EmulatorUdp:
    def __init__(self, serial, device_port):
        self.serial = serial
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.socket.bind(("127.0.0.1", 0))
        self.socket.settimeout(0.1)
        self.address = "127.0.0.1:" + str(self.socket.getsockname()[1])
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reserved:
            reserved.bind(("127.0.0.1", 0))
            self.redirect = reserved.getsockname()[1]
        self.stopped = threading.Event()
        self.error = None
        try:
            self.command("add", f"udp:{self.redirect}:{device_port}")
        except Exception:
            self.socket.close()
            raise
        self.worker = threading.Thread(target=self.forward, daemon=True)
        self.worker.start()

    def command(self, action, route):
        output = subprocess.check_output(["adb", "-s", self.serial, "emu", "redir", action, route], text=True, stderr=subprocess.STDOUT, timeout=10)
        if not output.strip().startswith("OK"):
            raise RuntimeError(output)
        return output

    def forward(self):
        client = None
        try:
            while not self.stopped.is_set():
                try:
                    data, source = self.socket.recvfrom(65535)
                except socket.timeout:
                    continue
                if client is None:
                    client = source
                if source == client:
                    self.socket.sendto(data, ("127.0.0.1", self.redirect))
                else:
                    # Emulator replies use an outbound NAT port, not its redirection port.
                    self.socket.sendto(data, client)
        except OSError as error:
            if not self.stopped.is_set():
                self.error = error
                self.stopped.set()

    def close(self):
        self.stopped.set()
        self.worker.join(timeout=1)
        self.socket.close()
        self.command("del", f"udp:{self.redirect}")
        if self.error:
            raise self.error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--port", required=True, type=int)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("port must be between 1 and 65535")
    route = EmulatorUdp(args.serial, args.port)
    print(route.address, flush=True)
    try:
        route.stopped.wait()
    except KeyboardInterrupt:
        pass
    finally:
        route.close()


if __name__ == "__main__":
    main()
