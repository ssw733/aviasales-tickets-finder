package config

type Config struct {
	Mysql struct {
		User     string
		Password string
		DBName   string
	}
	Aviasales struct {
		ApiToken string
	}
}

var Conf Config

func LoadConfig() Config {
	Conf.Mysql.User = ""
	Conf.Mysql.Password = ""
	Conf.Mysql.DBName = ""
	Conf.Aviasales.ApiToken = ""
	return Conf
}
