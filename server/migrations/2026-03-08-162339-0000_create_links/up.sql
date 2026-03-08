-- Your SQL goes here
CREATE TABLE `links`(
	`id` TEXT NOT NULL PRIMARY KEY,
	`url` TEXT NOT NULL,
	`orignal_hash` TEXT NOT NULL,
	`transcoded_hash` TEXT NOT NULL
);

