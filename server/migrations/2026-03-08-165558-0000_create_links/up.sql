-- Your SQL goes here
CREATE TABLE `links`(
	`id` TEXT NOT NULL PRIMARY KEY,
	`url` TEXT NOT NULL,
	`original_hash` TEXT,
	`transcoded_hash` TEXT
);

