// Copyright 2020 Snowfork
// SPDX-License-Identifier: LGPL-3.0-only

package core

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/viper"
	"golang.org/x/sync/errgroup"

	"github.com/snowfork/polkadot-ethereum/relayer/chain"
	"github.com/snowfork/polkadot-ethereum/relayer/chain/ethereum"
	"github.com/snowfork/polkadot-ethereum/relayer/chain/substrate"
	"github.com/snowfork/polkadot-ethereum/relayer/store"
)

type Relay struct {
	subChain chain.Chain
	ethChain chain.Chain
	database *store.Database
}

type Direction int

const (
	Bidirectional Direction = iota
	EthToSub
	SubToEth
)

type RelayConfig struct {
	Direction   Direction `mapstructure:"direction"`
	HeadersOnly bool      `mapstructure:"headers-only"`
}

type Config struct {
	Relay RelayConfig      `mapstructure:"relay"`
	Eth   ethereum.Config  `mapstructure:"ethereum"`
	Sub   substrate.Config `mapstructure:"substrate"`
}

func NewRelay() (*Relay, error) {
	config, err := LoadConfig()
	if err != nil {
		return nil, err
	}

	// TODO: integrate with config
	configJson := `
	{"db_config": {
			"dialect": "sqlite3",
			"db_path": "./tmp.db"
		}
	}`
	dbConfig := store.ParseConfigFromJson(configJson)

	db, err := store.PrepareDatabase(dbConfig)
	if err != nil {
		return nil, err
	}

	beefyMessages := make(chan store.DatabaseCmd, 1)
	logger := log.WithField("database", "Beefy")
	database := store.NewDatabase(db, beefyMessages, logger)

	subChain, err := substrate.NewChain(&config.Sub)
	if err != nil {
		return nil, err
	}

	ethChain, err := ethereum.NewChain(&config.Eth, database)
	if err != nil {
		return nil, err
	}

	direction := config.Relay.Direction
	headersOnly := config.Relay.HeadersOnly
	if direction == Bidirectional || direction == EthToSub {
		// channel for messages from ethereum
		var ethMessages chan []chain.Message
		if !headersOnly {
			ethMessages = make(chan []chain.Message, 1)
		}
		// channel for headers from ethereum (it's a blocking channel so that we
		// can guarantee that a header is forwarded before we send dependent messages)
		ethHeaders := make(chan chain.Header)

		err = subChain.SetReceiver(ethMessages, ethHeaders, beefyMessages)
		if err != nil {
			return nil, err
		}
		err = ethChain.SetSender(ethMessages, ethHeaders, beefyMessages)
		if err != nil {
			return nil, err
		}
	}

	if direction == Bidirectional || direction == SubToEth {
		// channel for messages from substrate
		var subMessages chan []chain.Message
		if !headersOnly {
			subMessages = make(chan []chain.Message, 1)
		}

		err := subChain.SetSender(subMessages, nil, beefyMessages)
		if err != nil {
			return nil, err
		}
		err = ethChain.SetReceiver(subMessages, nil, beefyMessages)
		if err != nil {
			return nil, err
		}
	}

	return &Relay{
		subChain: subChain,
		ethChain: ethChain,
		database: database,
	}, nil
}

func (re *Relay) Start() {

	ctx, cancel := context.WithCancel(context.Background())
	eg, ctx := errgroup.WithContext(ctx)

	// Ensure clean termination upon SIGINT, SIGTERM
	eg.Go(func() error {
		notify := make(chan os.Signal, 1)
		signal.Notify(notify, syscall.SIGINT, syscall.SIGTERM)

		select {
		case <-ctx.Done():
			return ctx.Err()
		case sig := <-notify:
			log.WithField("signal", sig.String()).Info("Received signal")
			cancel()

		}

		return nil
	})

	err := re.database.Start(ctx, eg)
	if err != nil {
		log.WithFields(log.Fields{
			"database": "Beefy",
			"error":    err,
		}).Error("Failed to start database")
		return
	}
	log.WithField("database", "Beefy").Info("Started database")

	// Short-lived channels that communicate initialization parameters
	// between the two chains. The chains close them after startup.
	subInit := make(chan chain.Init, 1)
	ethSubInit := make(chan chain.Init, 1)

	err = re.ethChain.Start(ctx, eg, subInit, ethSubInit)
	if err != nil {
		log.WithFields(log.Fields{
			"chain": re.ethChain.Name(),
			"error": err,
		}).Error("Failed to start chain")
		return
	}
	log.WithField("name", re.ethChain.Name()).Info("Started chain")
	defer re.ethChain.Stop()

	err = re.subChain.Start(ctx, eg, ethSubInit, subInit)
	if err != nil {
		log.WithFields(log.Fields{
			"chain": re.subChain.Name(),
			"error": err,
		}).Error("Failed to start chain")
		return
	}
	log.WithField("name", re.subChain.Name()).Info("Started chain")
	defer re.subChain.Stop()

	notifyWaitDone := make(chan struct{})

	go func() {
		err := eg.Wait()
		if err != nil && !errors.Is(err, context.Canceled) {
			log.WithField("error", err).Error("Encountered an unrecoverable failure")
		}
		close(notifyWaitDone)
	}()

	// Wait until a fatal error or signal is raised
	select {
	case <-notifyWaitDone:
		break
	case <-ctx.Done():
		// Goroutines are either shutting down or deadlocked.
		// Give them a few seconds...
		select {
		case <-time.After(3 * time.Second):
			break
		case _, stillWaiting := <-notifyWaitDone:
			if !stillWaiting {
				// All goroutines have ended
				return
			}
		}

		log.WithError(ctx.Err()).Error("Goroutines appear deadlocked. Killing process")
		re.ethChain.Stop()
		re.subChain.Stop()
		// re.database.Stop() // TODO: graceful shutdown

		relayProc, err := os.FindProcess(os.Getpid())
		if err != nil {
			log.WithError(err).Error("Failed to kill this process")
		}
		relayProc.Kill()
	}
}

func LoadConfig() (*Config, error) {
	var config Config
	err := viper.Unmarshal(&config)
	if err != nil {
		return nil, err
	}

	var direction = config.Relay.Direction
	if direction != Bidirectional &&
		direction != EthToSub &&
		direction != SubToEth {
		return nil, fmt.Errorf("'direction' has invalid value %d", direction)
	}

	// Load secrets from environment variables
	var value string
	var ok bool

	value, ok = os.LookupEnv("ARTEMIS_ETHEREUM_KEY")
	if !ok {
		return nil, fmt.Errorf("environment variable not set: ARTEMIS_ETHEREUM_KEY")
	}
	config.Eth.PrivateKey = strings.TrimPrefix(value, "0x")

	// TODO: auto populate contract addresses
	config.Eth.Contracts.PolkadotRelayChainBridge = "0x8cF6147918A5CBb672703F879f385036f8793a24"
	config.Eth.Contracts.ValidatorRegistry = "0xB1185EDE04202fE62D38F5db72F71e38Ff3E8305"
	// TODO: query from 'BLOCK_WAIT_PERIOD' on RelayBridgeLightClient contract
	config.Eth.BeefyBlockDelay = 5

	value, ok = os.LookupEnv("ARTEMIS_SUBSTRATE_KEY")
	if !ok {
		return nil, fmt.Errorf("environment variable not set: ARTEMIS_SUBSTRATE_KEY")
	}
	config.Sub.Parachain.PrivateKey = value
	config.Sub.Parachain.Endpoint = "ws://127.0.0.1:11144"
	config.Sub.Relaychain.Endpoint = "ws://127.0.0.1:9944"
	config.Sub.Relaychain.PrivateKey = "//Alice" // TODO: proper configuration

	return &config, nil
}
