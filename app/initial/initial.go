package initial

import (
	"aviasales/app/config"
	"database/sql"
	"fmt"

	_ "github.com/go-sql-driver/mysql"
)

func Run() {
	config.LoadConfig()
	db, _ := sql.Open("mysql", config.Conf.Mysql.User+":"+config.Conf.Mysql.Password+"@/"+config.Conf.Mysql.DBName)
	defer db.Close()

	createTables(db)
}

func createTables(db *sql.DB) {
	fmt.Println("Start creating tables...")
	cities, _ := db.Query("CREATE TABLE IF NOT EXISTS cities (code VARCHAR(3) NOT NULL, name VARCHAR(127) NOT NULL DEFAULT '', country_code VARCHAR(3) NOT NULL, coord_lat FLOAT NOT NULL DEFAULT 0, coord_lon FLOAT NOT NULL DEFAULT 0, PRIMARY KEY (code, country_code))")
	cities.Close()
	ways, _ := db.Query("CREATE TABLE IF NOT EXISTS ways (origin VARCHAR(3) NOT NULL, destination VARCHAR(3) NOT NULL, updated INT(1) NOT NULL DEFAULT 0, " +
		"PRIMARY KEY (origin, destination), " +
		"INDEX(origin)" + //Для удаления устаревших путей
		")")
	ways.Close()
	tickets, _ := db.Query("CREATE TABLE IF NOT EXISTS tickets (id INT PRIMARY KEY AUTO_INCREMENT, origin VARCHAR(3) NOT NULL, destination VARCHAR(3) NOT NULL, price INT NOT NULL, timestamp INT NOT NULL, link VARCHAR(1023) NOT NULL, " +
		"INDEX od (origin, destination), " + // Для удаления старых билетов
		"INDEX podtD (price, origin, destination, timestamp DESC), " + // Используется tickets в поиске билетов
		"INDEX dpt (destination, price, timestamp)" + // Используется tickets в поиске билетов
		")")
	tickets.Close()
	fmt.Println("Creating tables success")
}
