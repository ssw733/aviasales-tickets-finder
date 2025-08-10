package parser

import (
	"aviasales/app/config"
	"database/sql"
	"encoding/json"
	"fmt"
	"io/ioutil"
	"net/http"
	"strings"
	"time"

	_ "github.com/go-sql-driver/mysql"
)

type Cities []struct {
	Name struct {
		En string `json:"en"`
	} `json:"name_translations"`
	CityCode    string `json:"code"`
	CountryCode string `json:"country_code"`
	Coordinates struct {
		Lat float32 `json:"lat"`
		Lon float32 `json:"lon"`
	} `json:"coordinates"`
}

type Ways struct {
	Data map[string]struct {
		Flight map[string]int `json:"0"`
	} `json:"data"`
}

type Way struct {
	Id          int
	Origin      string
	Destination string
}

type Ways2 struct {
	Data []struct {
		Origin      string    `json:"origin"`
		Destination string    `json:"destination"`
		Departure   time.Time `json:"departure_at"`
		Price       int       `json:"price"`
		Link        string    `json:"link`
	} `json:"data"`
}

var Db *sql.DB
var Config config.Config

func init() {
	Config = config.LoadConfig()
	Db, _ = sql.Open("mysql", Config.Mysql.User+":"+Config.Mysql.Password+"@/"+Config.Mysql.DBName)
}

func ParseMeta() {
	parseCities()
	checkWays()
}

func ParsePrices() {
	for {
		parcePrices()
	}
}

func errorHandler(err error) {
	if err != nil {
		panic(err)
	}
}

func parseCities() {
	fmt.Println("Start parsing cities...")
	resp, err := http.Get("https://api.travelpayouts.com/data/ru/cities.json")
	errorHandler(err)
	defer resp.Body.Close()

	body, err := ioutil.ReadAll(resp.Body)
	errorHandler(err)
	var cities Cities
	json.Unmarshal(body, &cities)

	queryString := "REPLACE INTO cities (code, name, country_code, coord_lat, coord_lon) VALUES "
	fieldsCount := 5
	params := make([]interface{}, len(cities)*fieldsCount)

	for i, val := range cities {
		pos := i * fieldsCount
		params[pos+0] = val.CityCode
		params[pos+1] = val.Name.En
		params[pos+2] = val.CountryCode
		params[pos+3] = val.Coordinates.Lat
		params[pos+4] = val.Coordinates.Lon
		queryString += "(?,?,?,?,?),"
	}
	queryString = strings.TrimRight(queryString, ",") + ";"
	rows, err := Db.Query(queryString, params...)
	errorHandler(err)
	rows.Close()

	fmt.Println("Cities parsing success")
}

func checkWays() {
	fmt.Println("Start check ways...")

	rows, err := Db.Query("SELECT code FROM cities")
	errorHandler(err)
	for rows.Next() {
		var row string
		rows.Scan(&row)
		fmt.Println("Start " + row)
		checkWay(row)
	}
	rows.Close()
	fmt.Println("Check ways success")
}

func checkWay(way string) {
	client := &http.Client{}
	req, err := http.NewRequest(http.MethodGet, "http://api.travelpayouts.com/v1/prices/direct?origin="+way, nil)
	errorHandler(err)
	req.Header.Add("X-Access-Token", Config.Aviasales.ApiToken)
	resp, err := client.Do(req)
	defer resp.Body.Close()
	errorHandler(err)

	body, err := ioutil.ReadAll(resp.Body)
	errorHandler(err)
	var ways Ways
	json.Unmarshal(body, &ways)
	rows, err := Db.Query("DELETE FROM ways WHERE origin = ?", way)
	errorHandler(err)
	rows.Close()
	for key, val := range ways.Data {
		fmt.Println(way, key, val.Flight["price"])
		rows, err := Db.Query("REPLACE INTO ways (origin, destination) VALUES (?, ?)", way, key)
		errorHandler(err)
		rows.Close()
	}
}

func parcePrices() {
	rows, err := Db.Query("SELECT origin, destination FROM ways WHERE updated = 0")
	errorHandler(err)
	for rows.Next() {
		var row Way
		rows.Scan(&row.Origin, &row.Destination)
		fmt.Println("Start ", row)
		client := &http.Client{}
		req, err := http.NewRequest(http.MethodGet, "https://api.travelpayouts.com/aviasales/v3/prices_for_dates?origin="+row.Origin+"&destination="+row.Destination+"&direct=true&limit=1000", nil)
		errorHandler(err)
		req.Header.Add("X-Access-Token", Config.Aviasales.ApiToken)
		resp, err := client.Do(req)
		defer resp.Body.Close()
		errorHandler(err)
		body, err := ioutil.ReadAll(resp.Body)
		errorHandler(err)
		var ways Ways2
		json.Unmarshal(body, &ways)

		queryString := "REPLACE INTO tickets (origin, destination, price, timestamp, link) VALUES "
		fieldsCount := 5
		params := make([]interface{}, len(ways.Data)*fieldsCount)

		removeTickets, err := Db.Query("DELETE FROM tickets WHERE origin = ? AND destination = ?", row.Origin, row.Destination)
		errorHandler(err)
		removeTickets.Close()

		for i, val := range ways.Data {
			fmt.Println(val)
			pos := i * fieldsCount
			params[pos+0] = val.Origin
			params[pos+1] = val.Destination
			params[pos+2] = val.Price
			params[pos+3] = val.Departure.Unix()
			params[pos+4] = val.Link
			queryString += "(?,?,?,?,?),"
		}

		if len(params) > 0 {
			queryString = strings.TrimRight(queryString, ",") + ";"
			rows, err := Db.Query(queryString, params...)
			errorHandler(err)
			rows.Close()
		}

		res, err := Db.Query("UPDATE ways SET updated = 1 WHERE origin = ? AND destination = ?", row.Origin, row.Destination)
		errorHandler(err)
		res.Close()
	}
	rows.Close()
	rows2, err := Db.Query("UPDATE ways SET updated = 0")
	errorHandler(err)
	rows2.Close()
}
