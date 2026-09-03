package main

import (
	"flag"
	"fmt"
	"log"
	"net"
	"os"
	"os/signal"
	"strconv"
	"sync"
	"syscall"

	"github.com/pion/logging"
	"github.com/pion/turn/v5"
)

type observedPacketConn struct {
	net.PacketConn
	mu      sync.Mutex
	sources map[string]struct{}
}

func (conn *observedPacketConn) ReadFrom(buffer []byte) (int, net.Addr, error) {
	read, source, err := conn.PacketConn.ReadFrom(buffer)
	if err == nil && source != nil {
		key := source.String()
		conn.mu.Lock()
		_, seen := conn.sources[key]
		if !seen {
			conn.sources[key] = struct{}{}
		}
		conn.mu.Unlock()
		if !seen {
			log.Printf("TURN UDP client observed source=%s", source)
		}
	}
	return read, source, err
}

type observedListener struct {
	net.Listener
}

func (listener *observedListener) Accept() (net.Conn, error) {
	conn, err := listener.Listener.Accept()
	if err == nil {
		log.Printf("TURN TCP client accepted source=%s", conn.RemoteAddr())
	}
	return conn, err
}

func main() {
	publicIPText := flag.String("public-ip", "", "LAN IPv4 address advertised by relay candidates")
	authSecret := flag.String(
		"auth-secret",
		os.Getenv("REMOTE_TURN_SECRET"),
		"shared secret used by TURN REST credentials (defaults to REMOTE_TURN_SECRET)",
	)
	realm := flag.String("realm", "super-remote", "TURN authentication realm")
	tcpPort := flag.Int("tcp-port", 3478, "TURN TCP listening port")
	udpPort := flag.Int("udp-port", 3478, "TURN UDP listening port")
	minPort := flag.Int("min-port", 49160, "first UDP relay port")
	maxPort := flag.Int("max-port", 49200, "last UDP relay port")
	flag.Parse()

	publicIP := net.ParseIP(*publicIPText).To4()
	if publicIP == nil {
		log.Fatal("--public-ip must be an IPv4 address")
	}
	if *authSecret == "" {
		log.Fatal("--auth-secret is required")
	}
	if *tcpPort < 1 || *tcpPort > 65535 || *udpPort < 1 || *udpPort > 65535 {
		log.Fatal("--tcp-port and --udp-port must be between 1 and 65535")
	}
	if *minPort < 1 || *maxPort > 65535 || *minPort > *maxPort {
		log.Fatal("invalid relay port range")
	}

	tcpListenAddress := net.JoinHostPort("0.0.0.0", strconv.Itoa(*tcpPort))
	udpListenAddress := net.JoinHostPort("0.0.0.0", strconv.Itoa(*udpPort))
	tcpListener, err := net.Listen("tcp4", tcpListenAddress)
	if err != nil {
		log.Fatalf("listen TURN/TCP: %v", err)
	}
	udpListener, err := net.ListenPacket("udp4", udpListenAddress)
	if err != nil {
		_ = tcpListener.Close()
		log.Fatalf("listen TURN/UDP: %v", err)
	}

	newRelayGenerator := func() turn.RelayAddressGenerator {
		return &turn.RelayAddressGeneratorPortRange{
			RelayAddress: publicIP,
			Address:      "0.0.0.0",
			MinPort:      uint16(*minPort),
			MaxPort:      uint16(*maxPort),
			MaxRetries:   (*maxPort - *minPort) + 1,
		}
	}
	authLogger := logging.NewDefaultLeveledLoggerForScope(
		"turn-auth",
		logging.LogLevelInfo,
		os.Stdout,
	)
	authenticate := turn.LongTermTURNRESTAuthHandler(*authSecret, authLogger)
	server, err := turn.NewServer(turn.ServerConfig{
		Realm:       *realm,
		AuthHandler: authenticate,
		EventHandler: turn.EventHandler{
			OnAuth: func(srcAddr, _ net.Addr, _, username, _ string, method string, verdict bool) {
				if !verdict {
					log.Printf("TURN authentication rejected source=%s username=%q method=%s", srcAddr, username, method)
				}
			},
			OnAllocationCreated: func(srcAddr, _ net.Addr, protocol, userID, _ string, relayAddr net.Addr, _ int) {
				log.Printf("TURN allocation created source=%s user=%q protocol=%s relay=%s", srcAddr, userID, protocol, relayAddr)
			},
			OnAllocationDeleted: func(srcAddr, _ net.Addr, protocol, userID, _ string) {
				log.Printf("TURN allocation deleted source=%s user=%q protocol=%s", srcAddr, userID, protocol)
			},
			OnAllocationError: func(srcAddr, _ net.Addr, protocol, message string) {
				log.Printf("TURN allocation error source=%s protocol=%s error=%s", srcAddr, protocol, message)
			},
			OnPermissionCreated: func(srcAddr, _ net.Addr, _, userID, _ string, relayAddr net.Addr, peer net.IP) {
				log.Printf("TURN permission created source=%s user=%q relay=%s peer=%s", srcAddr, userID, relayAddr, peer)
			},
			OnChannelCreated: func(srcAddr, _ net.Addr, _, userID, _ string, relayAddr, peer net.Addr, _ uint16) {
				log.Printf("TURN channel created source=%s user=%q relay=%s peer=%s", srcAddr, userID, relayAddr, peer)
			},
		},
		PacketConnConfigs: []turn.PacketConnConfig{{
			PacketConn: &observedPacketConn{
				PacketConn: udpListener,
				sources:    make(map[string]struct{}),
			},
			RelayAddressGenerator: newRelayGenerator(),
		}},
		ListenerConfigs: []turn.ListenerConfig{{
			Listener:              &observedListener{Listener: tcpListener},
			RelayAddressGenerator: newRelayGenerator(),
		}},
	})
	if err != nil {
		_ = udpListener.Close()
		_ = tcpListener.Close()
		log.Fatalf("start TURN server: %v", err)
	}

	log.Printf(
		"TURN ready tcp=%s udp=%s relay=%s:%d-%d realm=%s",
		tcpListenAddress,
		udpListenAddress,
		publicIP.String(),
		*minPort,
		*maxPort,
		*realm,
	)
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	<-stop
	if err := server.Close(); err != nil {
		log.Printf("close TURN server: %v", err)
	}
	fmt.Println("TURN stopped")
}
