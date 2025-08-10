package main

import (
	"aviasales/app/config"
	"aviasales/app/initial"
	"aviasales/app/parser"
)

func main() {
	config.LoadConfig()
	initial.Run()
	parser.ParseMeta()
	parser.ParsePrices()
}
