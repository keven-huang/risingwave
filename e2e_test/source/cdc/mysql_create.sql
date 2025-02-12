CREATE TABLE products (
    id INTEGER NOT NULL AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    description VARCHAR(512)
) AUTO_INCREMENT = 101;

CREATE TABLE orders (
    order_id INTEGER NOT NULL AUTO_INCREMENT PRIMARY KEY,
    order_date DATETIME NOT NULL,
    customer_name VARCHAR(255) NOT NULL,
    price DECIMAL(10, 5) NOT NULL,
    product_id INTEGER NOT NULL,
    order_status BOOLEAN NOT NULL -- Whether order has been placed
) AUTO_INCREMENT = 10001;

CREATE TABLE mytable (
    v1 INTEGER NOT NULL PRIMARY KEY,
    v2 INTEGER NOT NULL,
    v3 VARCHAR(255) NOT NULL
);

DROP USER IF EXISTS 'dbz'@'%';
CREATE USER 'dbz'@'%' IDENTIFIED BY '123456';
GRANT SELECT, RELOAD, SHOW DATABASES, REPLICATION SLAVE, REPLICATION CLIENT ON *.* TO 'dbz'@'%';

CREATE TABLE tt3 (v1 int primary key, v2 timestamp);
