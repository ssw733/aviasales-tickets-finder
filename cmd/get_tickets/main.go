package main

import (
	"aviasales/app/config"
	"database/sql"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"strconv"
	"strings"
	"time"

	_ "github.com/go-sql-driver/mysql"
)

type Ticket struct {
	Id          int
	Origin      string
	Destination string
	Price       int
	Timestamp   int
	Link        string
}

type Flight struct {
	IdPath     string
	WayPath    string
	TotalPrice int
	Transfers  int
	Ticket     []Ticket
}

type Params struct {
	Origin         string `schema:"orig"`
	Destination    string `schema:"dest"`
	Transfer       int    `schema:"transfer"`
	TransferPeriod int    `schema:"transfer_period"`
	TimeDepMin     int    `schema:"timestamp_dep_min"`
	TimeDepMax     int    `schema:"timestamp_dep_max"`
	TimeArrMin     int    `schema:"timestamp_arr_min"`
	TimeArrMax     int    `schema:"timestamp_arr_max"`
}

type Response struct {
	Response any
	Msg      []string
}

func main() {
	http.HandleFunc("/get-tickets", getTicketsHandler)

	fmt.Println("Starting server on :8090...")
	if err := http.ListenAndServe(":8090", nil); err != nil {
		log.Fatalf("Server failed to start: %v", err)
	}
}

func getTicketsHandler(w http.ResponseWriter, r *http.Request) {
	var response Response
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Access-Control-Allow-Origin", "*")
	query := r.URL.Query()
	fmt.Println(r.RemoteAddr)
	var isValidQuery bool
	var params Params
	if len(query["origin"]) > 0 {
		params.Origin = query["origin"][0]
		isValidQuery = true
	}
	if len(query["destination"]) > 0 {
		params.Destination = query["destination"][0]
		isValidQuery = true
	}
	if len(query["transfer"]) > 0 {
		params.Transfer, _ = strconv.Atoi(query["transfer"][0])
	}
	if len(query["transferPeriod"]) > 0 {
		params.TransferPeriod, _ = strconv.Atoi(query["transferPeriod"][0])
	}
	if len(query["timeDepMin"]) > 0 {
		params.TimeDepMin, _ = strconv.Atoi(query["timeDepMin"][0])
	}
	if len(query["timeDepMax"]) > 0 {
		params.TimeDepMax, _ = strconv.Atoi(query["timeDepMax"][0])
	}
	if len(query["timeArrMin"]) > 0 {
		params.TimeArrMin, _ = strconv.Atoi(query["timeArrMin"][0])
	}
	if len(query["timeArrMax"]) > 0 {
		params.TimeArrMax, _ = strconv.Atoi(query["timeArrMax"][0])
	}
	if isValidQuery {
		response.Response = getTickets(params)
		response.Msg = append(response.Msg, "ok")
		responseJson, _ := json.Marshal(response)
		fmt.Fprint(w, string(responseJson))
	} else {
		response.Msg = append(response.Msg, "parameter origin or destination is empty")
		responseJson, _ := json.Marshal(response)
		http.Error(w, string(responseJson), http.StatusBadRequest)
	}
}

func errorHandler(err error) {
	if err != nil {
		panic(err)
	}
}

func getTickets(params Params) []Flight {
	fmt.Println(params)
	config.LoadConfig()
	db, err := sql.Open("mysql", config.Conf.Mysql.User+":"+config.Conf.Mysql.Password+"@/"+config.Conf.Mysql.DBName)
	errorHandler(err)
	defer db.Close()

	var condition, condition2 string
	var q, q2 []any
	queryString := "SELECT * FROM tickets WHERE 1 "

	if len(params.Origin) > 0 {
		condition += " AND origin = ? "
		q = append(q, params.Origin)
	}
	if len(params.Destination) > 0 {
		condition += " AND destination = ? "
		q = append(q, params.Destination)
		condition2 += " AND destination IN (?) "
		q2 = append(q2, params.Destination)
	}
	if params.TimeArrMin > 0 {
		condition += " AND timestamp > ? "
		q = append(q, params.TimeArrMin)
		condition2 += " AND timestamp > ? "
		q2 = append(q2, params.TimeArrMin)
	}
	if params.TimeArrMax > 0 {
		condition += " AND timestamp < ? "
		q = append(q, params.TimeArrMax)
		condition2 += " AND timestamp < ? "
		q2 = append(q2, params.TimeArrMax)
	}

	var flights []Flight

	if params.Transfer == 0 {
		queryString += condition
		rows, err := db.Query(queryString+" ORDER BY price LIMIT 1", q...)
		errorHandler(err)
		for rows.Next() {
			var ticket Ticket
			rows.Scan(&ticket.Id, &ticket.Origin, &ticket.Destination, &ticket.Price, &ticket.Timestamp, &ticket.Link)
			var flight Flight
			flight.Ticket = append(flight.Ticket, ticket)
			flight.TotalPrice = ticket.Price
			flight.WayPath = ticket.Destination
			flight.Transfers = 0
			flights = append(flights, flight)
		}
	} else {
		tableName := "temp_tickets" + strconv.FormatInt(time.Now().Unix(), 10)
		createTable, err := db.Query("CREATE TABLE IF NOT EXISTS " + tableName + " (id_path VARCHAR(255) PRIMARY KEY, way_path VARCHAR(255), origin VARCHAR(3), destination VARCHAR(3) NOT NULL, timestamp INT NOT NULL, iteration INT NOT NULL, total_price INT NOT NULL, " +
			"INDEX(origin)" +
			") ENGINE=MEMORY")
		errorHandler(err)
		createTable.Close()
		clearTable, err := db.Query("DELETE FROM " + tableName)
		errorHandler(err)
		clearTable.Close()
		minPrice := 9999999
		for t := range params.Transfer {
			if t == 0 {
				rows, err := db.Query("SELECT min(price) FROM tickets WHERE 1 "+condition, q...)
				errorHandler(err)
				for rows.Next() {
					rows.Scan(&minPrice)
				}
				rows.Close()
				q2 = append([]any{t, minPrice}, q2...)
				rows2, err := db.Query("INSERT INTO "+tableName+" (id_path, way_path, origin, destination, timestamp, iteration, total_price) "+
					"(SELECT id, destination, origin, destination, timestamp, ?, min(price) FROM tickets WHERE price <= ? "+condition2+" GROUP BY origin, destination)", q2...)
				errorHandler(err)
				rows2.Close()
			} else {
				/*rows, err := db.Query("INSERT INTO temp_tickets (id_path, way_path, origin, destination, timestamp, iteration, total_price) "+
				"SELECT CONCAT(t.id, '/', tt.id_path), CONCAT(t.destination, '/', tt.way_path), t.origin, tt.origin, t.timestamp, ?, tt.total_price+t.price FROM temp_tickets as tt  "+
				"INNER JOIN (SELECT t1.id, t1.origin, t1.destination, t1.timestamp, t1.price FROM tickets as t1 "+
				"INNER JOIN (SELECT origin, destination, MIN(price) as price FROM tickets WHERE price < ? GROUP BY origin, destination) as t2 "+
				"ON (t1.origin = t2.origin AND t1.destination = t2.destination AND t1.price = t2.price)) as t "+
				"ON (tt.origin = t.destination) "+
				"WHERE tt.iteration = ? AND t.price + tt.total_price < ? AND t.timestamp < tt.timestamp "+
				"GROUP BY t.origin, t.destination", t, minPrice, t-1, minPrice)*/
				rows, err := db.Query("INSERT INTO "+tableName+" (id_path, way_path, origin, destination, timestamp, iteration, total_price) "+
					"SELECT CONCAT(t.id, '/', tt.id_path), CONCAT(t.destination, '/', tt.way_path), t.origin, tt.origin, t.timestamp, ?, tt.total_price+t.price FROM "+tableName+" as tt  "+
					"INNER JOIN tickets as t ON (tt.origin = t.destination AND tt.iteration = ? AND t.timestamp < tt.timestamp AND (t.timestamp + ?) > tt.timestamp AND tt.total_price+t.price <= ?) "+
					"INNER JOIN tickets as t2 USE INDEX (podtD) ON (t.origin = t2.origin AND t.destination = t2.destination AND t.price = t2.price AND t.timestamp = t2.timestamp) "+
					"GROUP BY t2.origin, t2.destination", t, t-1, params.TransferPeriod*24*60*60, minPrice)
				errorHandler(err)
				rows.Close()
				rows, err = db.Query("SELECT min(total_price) FROM "+tableName+" WHERE origin IN (?)", params.Origin)
				errorHandler(err)
				for rows.Next() {
					rows.Scan(&minPrice)
				}
				rows.Close()
			}
			fmt.Println("Transfer ", t, "price: ", minPrice, "Р")
		}

		rows, err := db.Query("SELECT id_path, way_path, iteration, total_price FROM "+tableName+" WHERE origin IN (?) ORDER BY total_price", params.Origin)
		errorHandler(err)
		for rows.Next() {
			var flight Flight
			rows.Scan(&flight.IdPath, &flight.WayPath, &flight.Transfers, &flight.TotalPrice)
			tickets := strings.Split(flight.IdPath, "/")
			var ticks []Ticket
			for _, ticket_id := range tickets {
				rows2, err := db.Query("SELECT id, origin, destination, timestamp, price, link FROM tickets WHERE id = ? LIMIT 1", ticket_id)
				errorHandler(err)
				for rows2.Next() {
					var tick Ticket
					rows2.Scan(&tick.Id, &tick.Origin, &tick.Destination, &tick.Timestamp, &tick.Price, &tick.Link)
					tick.Link = "https://www.aviasales.ru" + tick.Link
					ticks = append(ticks, tick)
				}
				rows2.Close()
			}
			flight.Ticket = ticks
			flights = append(flights, flight)
		}
		rows.Close()
		rows3, err := db.Query("DROP TABLE " + tableName)
		errorHandler(err)
		rows3.Close()
	}
	return flights
}
