CREATE TABLE squirrel_items (id integer PRIMARY KEY, label text, score integer);
INSERT INTO squirrel_items VALUES (1, 'alpha', 10), (2, 'beta', 20);
UPDATE squirrel_items SET score = score + 1 WHERE id = 1;
SELECT id, score FROM squirrel_items ORDER BY id;
