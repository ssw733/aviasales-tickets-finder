package main

import (
	"aviasales/app/config"
	"aviasales/app/parser"
)

func main() {
	config.LoadConfig()
	for {
		parser.ParsePrices()
	}
}
